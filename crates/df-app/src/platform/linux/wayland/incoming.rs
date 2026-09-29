//! What the Wayland data device reads off a drag coming in: whether it is
//! this window's own, which of its types to ask for, and the paths in what
//! comes back.
//!
//! Moved here from `crate::dnd`, which keeps what a drag is on every
//! platform — the zones, the ghost, what a drag out offers. Only the Wayland
//! device is handed an incoming drag's types (winit's drop on macOS and
//! Windows hands over paths), so only Linux reads them.

use std::path::PathBuf;

use crate::dnd::self_mime;

/// Was this offer started by *this* window?
///
/// An exact match, so another delightfile's drag is external — which it is.
pub fn is_ours(offered: &[String]) -> bool {
    let mine = self_mime();
    offered.iter().any(|mime| mime == mine)
}

/// Which of an incoming drag's offered mimes to ask for, most useful first.
///
/// The same preference order [`crate::clipboard::choose_offer`] uses for a
/// paste, and for the same reason: a file manager asked, so a list of files
/// beats a picture of one.
pub fn wanted_mime(offered: &[String]) -> Option<String> {
    let find = |wanted: &str| {
        offered
            .iter()
            .find(|mime| mime.eq_ignore_ascii_case(wanted))
            .cloned()
    };
    find("text/uri-list")
        .or_else(|| find("text/x-moz-url"))
        .or_else(|| {
            offered
                .iter()
                .find(|mime| mime.starts_with("text/plain"))
                .cloned()
        })
}

/// Turn what a drop handed over into paths.
///
/// `text/x-moz-url` is Firefox's spelling — UTF-16, alternating URL and title
/// lines — and it is not a file drag, so it comes back empty rather than as a
/// path made of mojibake. Everything else is parsed as a uri-list, which is
/// lenient enough to also read the plain-text fallback: a bare `/tmp/a.txt`
/// line is not a `file://` URI and is dropped, so a plain-text drag of a path
/// is handled by the `path_lines` half.
pub fn paths_from(mime: &str, bytes: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    if mime.eq_ignore_ascii_case("text/uri-list") {
        return crate::clipboard::parse_uri_list(&text);
    }
    let uris = crate::clipboard::parse_uri_list(&text);
    if !uris.is_empty() {
        return uris;
    }
    // A plain-text drag of one or more absolute paths. Relative ones are
    // refused: this program has no idea what they would be relative *to*, and
    // guessing at the process working directory is how a drop writes somewhere
    // nobody was looking (the same rail `plan_paste` puts on a rename).
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dnd::{offer, SELF_MIME};

    /// **The multi-window rule** (PLAN §2, [`crate::window`]): "ours" means
    /// *this* window's drag, not any delightfile's. A drag from another
    /// window is another program's drag as far as this one is concerned —
    /// which is what makes window→window drops light the drop ring and paste
    /// like any other external drop.
    #[test]
    fn another_windows_drag_is_not_this_windows_drag() {
        let mine = offer(&[PathBuf::from("/tmp/a")])
            .into_iter()
            .map(|(mime, _)| mime)
            .collect::<Vec<_>>();
        assert!(is_ours(&mine));
        // The same program, a different process: the pid is the only thing
        // that differs, and it is enough.
        let sibling: Vec<String> = mine
            .iter()
            .map(|mime| {
                if mime.starts_with(SELF_MIME) {
                    format!("{SELF_MIME};pid={}", std::process::id() + 1)
                } else {
                    mime.clone()
                }
            })
            .collect();
        assert!(!is_ours(&sibling));
        // An older delightfile's unqualified marker is not ours either, and
        // neither is a drag from anything else.
        assert!(!is_ours(&[SELF_MIME.to_string()]));
        assert!(!is_ours(&["text/uri-list".to_string()]));
        assert!(!is_ours(&[]));
        // …and it is still a *file* drag, so the receiving window knows what
        // to ask for.
        assert_eq!(
            wanted_mime(&sibling),
            Some("text/uri-list".to_string()),
            "a sibling window's drag must still be readable as files"
        );
    }

    /// The incoming half: which mime to ask for, and what comes back.
    #[test]
    fn an_incoming_drag_is_read_as_files() {
        let types = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            wanted_mime(&types(&["text/plain", "text/uri-list"])),
            Some("text/uri-list".to_string())
        );
        assert_eq!(
            wanted_mime(&types(&["text/plain;charset=utf-8"])),
            Some("text/plain;charset=utf-8".to_string())
        );
        assert_eq!(wanted_mime(&types(&["image/png"])), None);
        assert_eq!(wanted_mime(&[]), None);

        let list = b"file:///tmp/a.txt\r\nfile:///tmp/b%20c.txt\r\n";
        assert_eq!(
            paths_from("text/uri-list", list),
            vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b c.txt")]
        );
        // A plain-text drop of URIs is still a file drag…
        assert_eq!(
            paths_from("text/plain", b"file:///tmp/a.txt"),
            vec![PathBuf::from("/tmp/a.txt")]
        );
        // …and one of bare absolute paths is too.
        assert_eq!(
            paths_from("text/plain", b"/tmp/a.txt\n/tmp/b.txt\n"),
            vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
        );
        // A relative path, or a URL, is not something to paste.
        assert!(paths_from("text/plain", b"notes.txt").is_empty());
        assert!(paths_from("text/plain", b"https://example.com/x").is_empty());
        assert!(paths_from("text/uri-list", b"https://example.com/x").is_empty());
    }
}
