//! The system clipboard (PLAN §7.4): a native port of Brian's yazi
//! `clipboard.sh` and `copy-text.sh`.
//!
//! ## The protocol first, `wl-copy` second
//!
//! PLAN §7.4 says "all via Wayland data-control/data-device", and that is now
//! where a copy goes: [`crate::wayland`]'s thread owns a `wl_data_source`,
//! answers `set_selection` with it and serves the bytes over a pipe when some
//! other application pastes — the same machinery, and the same 5 s send
//! timeout, drag-out already uses. A clipboard *source* on Wayland has to stay
//! alive to serve its data, and that thread is the thing that stays alive.
//!
//! [`copy`], [`offered_types`] and [`paste`] below are what is left of the old
//! answer, and they are now the **fallback**: a session with no seat and no
//! data device (X11, a compositor without `wl_data_device_manager`, a registry
//! that did not answer) still copies and pastes by shelling out, exactly as
//! yazi's scripts do. Two things that used to be arguments for `wl-copy` are
//! worth writing down as the cost of the change:
//!
//! - `wl-copy` forks a server and the selection survives the file manager
//!   exiting. Ours does not: quitting hands the selection back, which is what
//!   every application that owns its own clipboard does, and what the protocol
//!   is shaped for. The fallback below is now the same shape — it runs
//!   `--foreground` and the window owns the process — so both paths behave
//!   alike and neither leaves a stranger's process holding this program's
//!   bytes after it has quit.
//! - In exchange, a failure is now *visible* on both paths. `wl-copy`'s *forked*
//!   server could fail after the parent exited zero and the toast had already
//!   said "Copied"; the protocol path says so only once the compositor has
//!   taken the selection, and the fallback only once the server it started has
//!   lived long enough to be serving one.
//!
//! ## What is pure and what is not
//!
//! The *decisions* — which branch a file takes, what a `file://` URI looks like
//! for a name with a newline in it, whether the size cap has been hit — are
//! pure functions with tests. Only the two `Command` calls at the bottom touch
//! the world.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The largest file whose *contents* go on the clipboard. Past this only a
/// `file://` reference does.
///
/// 50 MiB is `clipboard.sh`'s number, and it is a number about the *receiver*:
/// a paste hands the whole selection over a pipe, so a 2 GB video copied by
/// value is two gigabytes through a socket into an application that probably
/// wanted the path. Past the cap the URI is not a degradation, it is the right
/// answer — every file manager and file dialog in existence reads `text/uri-list`.
pub const SIZE_CAP: u64 = 50 * 1024 * 1024;

/// How a file goes onto the clipboard.
///
/// The three branches are `clipboard.sh`'s, in its order, and the order is
/// load-bearing: the size cap is checked *before* the mime, so a 300 MB PNG is
/// a reference and not an attempt to push 300 MB of pixels through a pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// One image, offered with its real mime so a paste into an image editor or
    /// a chat window gets pixels rather than a filename.
    Image(&'static str),
    /// One text-like file, offered as `text/plain` — universal compatibility,
    /// which is the whole point of not offering `text/x-shellscript`.
    Text,
    /// Everything else: binaries, documents, media, anything over the cap, and
    /// *always* a multi-file selection.
    Uris,
}

/// Mimes that are text on disk and worth pasting as text.
///
/// The union of the two scripts' lists. `text/*` covers most of it; the rest
/// are the `application/…` spellings that `file(1)` gives to things that are
/// plainly text — and df-core's sniffer answers with the same family names, so
/// a Rust file arriving as `text/rust` matches the prefix and a `package.json`
/// arriving as `application/json` matches the list.
const TEXT_LIKE: &[&str] = &[
    "application/json",
    "application/ld+json",
    "application/xml",
    "application/javascript",
    "application/x-shellscript",
    "application/x-yaml",
    "application/yaml",
    "application/toml",
    "application/x-toml",
    // `file(1)` reports an empty file as `inode/x-empty`, and copying nothing
    // as text is more useful than copying a URI to an empty file.
    "inode/x-empty",
    "inode/empty",
];

/// Is this mime worth putting on the clipboard as plain text?
pub fn is_text_like(mime: &str) -> bool {
    let base = mime.split(';').next().unwrap_or(mime).trim();
    base.starts_with("text/") || TEXT_LIKE.contains(&base)
}

pub fn is_image(mime: &str) -> bool {
    mime.split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .starts_with("image/")
}

/// Which branch a copy takes. The whole of `clipboard.sh`'s `case`, as one
/// function.
///
/// `count` is how many files were selected: more than one is *always* a URI
/// list, because there is no such thing as the concatenation of two files.
pub fn branch_for(count: usize, mime: &str, size: u64) -> Branch {
    if count != 1 {
        return Branch::Uris;
    }
    if size > SIZE_CAP {
        return Branch::Uris;
    }
    if is_image(mime) {
        // The mime is handed back so the offer carries the *real* type; a PNG
        // offered as `image/jpeg` is a paste that fails in the other program.
        return Branch::Image(static_image_mime(mime));
    }
    if is_text_like(mime) {
        return Branch::Text;
    }
    Branch::Uris
}

/// A `&'static str` for an image mime, so [`Branch`] can stay `Copy`.
///
/// df-core's sniffer already answers with `&'static str`s from its own table,
/// so the common path is a hit here; anything else falls back to the generic
/// type, which every compositor accepts as an offer even if fewer applications
/// ask for it.
fn static_image_mime(mime: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/avif",
        "image/heic",
        "image/heif",
        "image/tiff",
        "image/bmp",
        "image/jxl",
        "image/svg+xml",
        "image/vnd.microsoft.icon",
    ];
    let base = mime.split(';').next().unwrap_or(mime).trim();
    KNOWN
        .iter()
        .copied()
        .find(|k| *k == base)
        .unwrap_or("image/png")
}

/// What a branch is called in a toast: "Copied image (PNG)".
pub fn image_label(mime: &str) -> String {
    let sub = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or("image");
    // `svg+xml` reads as SVG — the format is the part before the `+`, the rest
    // is how it is written down. A vendor tree (`vnd.microsoft.icon`) is the
    // other way round: everything before the last dot is provenance, and the
    // last segment is the name a person would use.
    let name = if let Some(vendor) = sub.strip_prefix("vnd.") {
        vendor.rsplit('.').next().unwrap_or(vendor)
    } else {
        sub.split('+').next().unwrap_or(sub)
    };
    name.to_uppercase()
}

/// The extension to save a pasted clipboard image under.
pub fn image_extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or(mime).trim() {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        // PNG is the fallback rather than the mime's own subtype: an unknown
        // image type saved as `clipboard.image%2Fx-weird` is a file nothing can
        // open, and the overwhelming majority of clipboard images are PNG.
        _ => "png",
    }
}

// ── `text/uri-list` ─────────────────────────────────────────────────────────

/// Characters that may appear unescaped in a URI path.
///
/// RFC 3986's *unreserved* set plus the sub-delims and the separators that are
/// legal in a path segment. Everything else — spaces, `#`, `?`, control
/// characters, and every byte of a UTF-8 name — is percent-encoded, because a
/// file called `report #3?.txt` otherwise arrives at the other application as a
/// path with a fragment and a query on the end of it.
fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// One path as a `file://` URI.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if unreserved(byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The other direction. `None` for anything that is not a local `file://` URI —
/// a `http://` in a uri-list is somebody's browser drag, and pasting it as a
/// file would be a lie.
pub fn parse_file_uri(text: &str) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let text = text.trim();
    // `file:///path` is the spec's spelling (empty authority); `file://path`
    // is what a great many programs — including `clipboard.sh` — actually
    // write. Both are accepted, and only a *remote* authority is refused.
    let rest = text.strip_prefix("file://")?;
    if let Some(local) = rest.strip_prefix("localhost/") {
        // `file://localhost/tmp/x` — rare, legal, and one line to honour.
        return parse_file_uri(&format!("file:///{local}"));
    }
    // Anything else after the `//` is a *remote* authority, and a remote path
    // is not a file this program can paste.
    let rest = rest.strip_prefix('/').map(|_| rest)?;
    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

/// The `text/uri-list` payload for a selection.
///
/// CRLF line endings, because that is what RFC 2483 says a `text/uri-list` is,
/// and applications that split on it exist.
pub fn uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        out.push_str(&file_uri(path));
        out.push_str("\r\n");
    }
    out
}

/// Read a `text/uri-list` back. Comment lines (`#`) are skipped, per the spec.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(parse_file_uri)
        .collect()
}

// ── What a selection offers ─────────────────────────────────────────────────

/// The mime types one copy is announced under.
///
/// A caller says what it has — `Some("image/png")`, `Some("text/uri-list")`, or
/// `None` for text — and gets the list the `wl_data_source` offers, most
/// specific first. Only text has more than one name: `text/plain;charset=utf-8`
/// is what modern toolkits ask for and bare `text/plain` is what older ones
/// ask for, and the same bytes answer both because the bytes *are* UTF-8. A
/// typed copy is offered under its own type and nothing else — a PNG announced
/// as `text/plain` is a paste that produces mojibake in whatever asked.
pub fn offer_mimes(mime: Option<&str>) -> Vec<String> {
    match mime {
        Some(mime) => vec![mime.to_string()],
        None => vec![
            "text/plain;charset=utf-8".to_string(),
            "text/plain".to_string(),
        ],
    }
}

// ── The fallback calls that touch the world ─────────────────────────────────

/// What can go wrong, in the two shapes the caller words differently.
#[derive(Debug)]
pub enum ClipError {
    /// `wl-copy`/`wl-paste` are not on `PATH` — and this path is only reached
    /// when the data device was not there either, so the copy did not happen
    /// and it says so in red like any other failure.
    Missing(&'static str),
    Failed(String),
}

impl std::fmt::Display for ClipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipError::Missing(tool) => {
                write!(f, "{tool} is not installed — install wl-clipboard")
            }
            ClipError::Failed(message) => f.write_str(message),
        }
    }
}

/// Put `bytes` on the clipboard, offered as `mime` (or as plain text when it is
/// `None`) — the **fallback** copy, for a session with no data device.
///
/// Returns the running `wl-copy`, which the caller owns and must eventually
/// [`reap`]. `--foreground` is the whole point: without it `wl-copy` forks a
/// server and the parent exits zero *before* that server has taken the
/// selection, so a successful `wait` here says nothing at all about whether the
/// copy happened — which is exactly the false "Copied" this program used to
/// show. With it, the process that is serving the selection is the process this
/// function hands back, and it being alive a moment later is evidence.
pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Child, ClipError> {
    let mut command = Command::new("wl-copy");
    command.arg("--foreground");
    if let Some(mime) = mime {
        command.arg("--type").arg(mime);
    }
    // **No `--trim-newline`.** The bytes handed over are the bytes offered:
    // trimming would eat the final newline of a copied file (changing its
    // contents) and the terminating CRLF of a `text/uri-list` (making it
    // invalid). The scripts this is a port of do not trim either.
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-copy"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    // Every failure from here on has a process attached to it, and each one
    // reaps it: an error return that left `wl-copy` running would be a stranger
    // holding the clipboard, and one that left it exited but unwaited would be
    // a zombie for the rest of the session.
    let Some(mut stdin) = child.stdin.take() else {
        // Piped a line ago, so this cannot happen — and a copy that silently
        // succeeded with nothing written is the one way it could go wrong that
        // the user would never see, so it is an error rather than an `if let`
        // with no `else`.
        reap(&mut child);
        return Err(ClipError::Failed(
            "wl-copy gave us nothing to write to".to_string(),
        ));
    };
    let written = stdin.write_all(bytes);
    // Closed before anything waits on the process, or `wl-copy` sits reading a
    // pipe nobody is going to close.
    drop(stdin);
    if let Err(e) = written {
        reap(&mut child);
        return Err(ClipError::Failed(e.to_string()));
    }
    Ok(child)
}

/// Stop a `wl-copy` we are done with and collect it.
///
/// Both halves. `kill` alone leaves a zombie until this process exits, and
/// `wait` alone would block forever on a `--foreground` server that is doing
/// exactly what it was asked to do. Called when a newer copy replaces this one
/// and when the window quits.
pub fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The mime types the clipboard is currently offering, most specific first —
/// `wl-paste --list-types`. The fallback for [`crate::wayland`]'s own mirror of
/// the selection's offer.
pub fn offered_types() -> Result<Vec<String>, ClipError> {
    let output = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-paste"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    if !output.status.success() {
        // An empty clipboard is an exit code, not a crash: "nothing to paste"
        // is a legitimate answer and the caller says so in a notice.
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// The clipboard's bytes, as `mime`.
pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError> {
    let output = Command::new("wl-paste")
        .arg("--no-newline")
        .arg("--type")
        .arg(mime)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-paste"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    if !output.status.success() {
        return Err(ClipError::Failed("nothing on the clipboard".to_string()));
    }
    Ok(output.stdout)
}

/// Which offered type this paste should ask for, and what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// A list of files: paste them the way `p` pastes a yank.
    Files,
    /// An image: save it as a file in the current directory.
    Image(String),
    /// Text: save it as a `.txt`.
    Text(String),
}

impl Offer {
    /// The mime to actually ask the clipboard for.
    ///
    /// The image and text branches carry the offered spelling — asking for
    /// `image/png` when the owner said `image/PNG` gets nothing back — and the
    /// file branch is the one type it was chosen by.
    pub fn mime(&self) -> &str {
        match self {
            Offer::Files => "text/uri-list",
            Offer::Image(mime) | Offer::Text(mime) => mime,
        }
    }
}

/// The type to ask for when what is wanted is **text** — a prompt's `Ctrl+v`.
///
/// A different question from [`choose_offer`]'s, and it has to be: a caret
/// takes characters, so a screenshot's `image/png` is nothing it can use and
/// there is no sense in writing a file out to answer a keystroke in a filter
/// box. `text/plain` first, then anything else in the `text/` family — which
/// includes `text/uri-list`, and a path *is* text worth typing into a rename.
pub fn text_offer(types: &[String]) -> Option<String> {
    types
        .iter()
        .find(|t| t.starts_with("text/plain"))
        .or_else(|| types.iter().find(|t| t.starts_with("text/")))
        .or_else(|| types.iter().find(|t| is_text_like(t)))
        .cloned()
}

/// Pick the best offer out of `wl-paste --list-types`.
///
/// Files first, because a file manager asked: a screenshot tool that offers
/// both a `text/uri-list` and an `image/png` means "here is a file", and
/// writing a second copy of it into the directory would be the wrong answer to
/// a paste. Then images, then text — an image offered alongside `text/html` is
/// a picture, not a document.
pub fn choose_offer(types: &[String]) -> Option<Offer> {
    let has = |wanted: &str| types.iter().any(|t| t.eq_ignore_ascii_case(wanted));
    if has("text/uri-list") {
        return Some(Offer::Files);
    }
    if let Some(image) = types.iter().find(|t| is_image(t)) {
        return Some(Offer::Image(image.clone()));
    }
    // `text/plain;charset=utf-8` is the usual spelling, so the match is on the
    // family rather than on the exact string.
    if let Some(text) = types
        .iter()
        .find(|t| t.starts_with("text/plain"))
        .or_else(|| types.iter().find(|t| t.starts_with("text/")))
    {
        return Some(Offer::Text(text.clone()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch table, one row per line of `clipboard.sh`'s `case`.
    #[test]
    fn the_branch_table_matches_the_script_it_was_ported_from() {
        let small = 1024;
        assert_eq!(
            branch_for(1, "image/png", small),
            Branch::Image("image/png")
        );
        assert_eq!(
            branch_for(1, "image/jpeg", small),
            Branch::Image("image/jpeg")
        );
        assert_eq!(branch_for(1, "text/plain", small), Branch::Text);
        // df-core's sniffer names source files by language; they are still text.
        assert_eq!(branch_for(1, "text/rust", small), Branch::Text);
        assert_eq!(branch_for(1, "application/json", small), Branch::Text);
        assert_eq!(
            branch_for(1, "application/x-shellscript", small),
            Branch::Text
        );
        assert_eq!(branch_for(1, "inode/x-empty", small), Branch::Text);
        // Documents, media and unknown binaries are all references.
        assert_eq!(branch_for(1, "application/pdf", small), Branch::Uris);
        assert_eq!(branch_for(1, "video/mp4", small), Branch::Uris);
        assert_eq!(branch_for(1, "audio/flac", small), Branch::Uris);
        assert_eq!(
            branch_for(1, "application/octet-stream", small),
            Branch::Uris
        );
        // A charset parameter must not defeat the match.
        assert_eq!(
            branch_for(1, "text/plain;charset=utf-8", small),
            Branch::Text
        );
    }

    /// The cap is checked before the mime: a huge image is a reference.
    #[test]
    fn anything_over_the_cap_is_copied_by_reference() {
        assert_eq!(branch_for(1, "image/png", SIZE_CAP + 1), Branch::Uris);
        assert_eq!(branch_for(1, "text/plain", SIZE_CAP + 1), Branch::Uris);
        // …and exactly at the cap it still goes by value, as the script's
        // `-gt` says.
        assert_eq!(
            branch_for(1, "image/png", SIZE_CAP),
            Branch::Image("image/png")
        );
    }

    /// More than one file is always a list, whatever the files are.
    #[test]
    fn a_multi_file_selection_is_always_a_uri_list() {
        assert_eq!(branch_for(2, "image/png", 10), Branch::Uris);
        assert_eq!(branch_for(9, "text/plain", 10), Branch::Uris);
        // …and so is none, which is the empty-directory case.
        assert_eq!(branch_for(0, "text/plain", 10), Branch::Uris);
    }

    /// The gnarly-name round trip: spaces, `#`, `?`, quotes, newlines and
    /// non-ASCII all survive an encode and a decode.
    #[test]
    fn file_uris_round_trip_through_the_worst_names_there_are() {
        let names = [
            "/tmp/plain.txt",
            "/tmp/with space.txt",
            "/tmp/report #3?.txt",
            "/tmp/100% done.txt",
            "/tmp/we're \"here\".txt",
            "/tmp/two\nlines.txt",
            "/tmp/café/naïve — em dash.txt",
            "/tmp/a+b&c=d.txt",
            "/tmp/back\\slash.txt",
        ];
        for name in names {
            let path = PathBuf::from(name);
            let uri = file_uri(&path);
            assert!(
                !uri.contains(' ') && !uri.contains('\n'),
                "unescaped whitespace in {uri}"
            );
            assert_eq!(parse_file_uri(&uri), Some(path.clone()), "{uri}");
        }
    }

    /// A whole list survives, and only `file://` lines come back.
    #[test]
    fn a_uri_list_round_trips_and_drops_what_is_not_a_file() {
        let paths = vec![
            PathBuf::from("/tmp/one.txt"),
            PathBuf::from("/tmp/two files.txt"),
        ];
        let text = uri_list(&paths);
        assert!(text.ends_with("\r\n"), "RFC 2483 lines end CRLF");
        assert_eq!(parse_uri_list(&text), paths);

        let mixed = "# a comment\r\nfile:///tmp/a.txt\r\nhttps://example.com/b\r\n\r\n";
        assert_eq!(parse_uri_list(mixed), vec![PathBuf::from("/tmp/a.txt")]);
    }

    /// The spellings other programs actually write.
    #[test]
    fn the_lenient_uri_spellings_are_accepted() {
        assert_eq!(
            parse_file_uri("file:///tmp/a.txt"),
            Some(PathBuf::from("/tmp/a.txt"))
        );
        // What `clipboard.sh` itself emits: `file://` plus an absolute path.
        assert_eq!(
            parse_file_uri("file:///tmp/a.txt\r"),
            Some(PathBuf::from("/tmp/a.txt"))
        );
        assert_eq!(
            parse_file_uri("file://localhost/tmp/a.txt"),
            Some(PathBuf::from("/tmp/a.txt"))
        );
        // A remote authority is refused rather than silently made local.
        assert_eq!(parse_file_uri("file://other-host/tmp/a.txt"), None);
        assert_eq!(parse_file_uri("/tmp/a.txt"), None);
        assert_eq!(parse_file_uri("https://example.com"), None);
    }

    /// The paste side's preference order.
    #[test]
    fn files_beat_images_beat_text() {
        let types = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            choose_offer(&types(&["text/uri-list", "image/png", "text/plain"])),
            Some(Offer::Files)
        );
        assert_eq!(
            choose_offer(&types(&["image/png", "text/html", "text/plain"])),
            Some(Offer::Image("image/png".to_string()))
        );
        assert_eq!(
            choose_offer(&types(&["text/plain;charset=utf-8"])),
            Some(Offer::Text("text/plain;charset=utf-8".to_string()))
        );
        assert_eq!(choose_offer(&[]), None);
        assert_eq!(choose_offer(&types(&["application/x-weird"])), None);
    }

    /// What the `wl_data_source` announces, per branch.
    #[test]
    fn a_selection_offers_both_text_spellings_and_one_of_everything_else() {
        assert_eq!(
            offer_mimes(None),
            vec![
                "text/plain;charset=utf-8".to_string(),
                "text/plain".to_string()
            ]
        );
        // A typed copy is offered under exactly its own type: a PNG announced
        // as text is a paste that lands as mojibake.
        assert_eq!(
            offer_mimes(Some("image/png")),
            vec!["image/png".to_string()]
        );
        assert_eq!(
            offer_mimes(Some("text/uri-list")),
            vec!["text/uri-list".to_string()]
        );
        // Every branch of a `Y` produces an offer some other application can
        // find: the image and uri-list branches by their own name, the text
        // branch by the two names a paste asks for.
        for (mime, wanted) in [
            (Some("image/png"), "image/png"),
            (Some("text/uri-list"), "text/uri-list"),
            (None, "text/plain"),
            (None, "text/plain;charset=utf-8"),
        ] {
            assert!(
                offer_mimes(mime).iter().any(|m| m == wanted),
                "{mime:?} should be askable as {wanted}"
            );
        }
    }

    #[test]
    fn image_labels_and_extensions_read_like_a_person_wrote_them() {
        assert_eq!(image_label("image/png"), "PNG");
        assert_eq!(image_label("image/jpeg"), "JPEG");
        assert_eq!(image_label("image/svg+xml"), "SVG");
        assert_eq!(image_label("image/vnd.microsoft.icon"), "ICON");
        assert_eq!(image_extension("image/jpeg"), "jpg");
        assert_eq!(image_extension("image/png"), "png");
        assert_eq!(image_extension("image/x-unheard-of"), "png");
    }
}
