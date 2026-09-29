//! What the macOS pasteboard calls the things this program copies and
//! pastes, and what they are called here.
//!
//! The clipboard code above the platform speaks mime types — `text/uri-list`,
//! `text/plain;charset=utf-8`, `image/png` — because that is what the Wayland
//! data device it was written against speaks. The pasteboard speaks uniform
//! type identifiers instead (`public.file-url`, `public.utf8-plain-text`,
//! `public.png`). The translation is a table, and a table is pure, so it
//! lives here rather than beside the AppKit calls: compiled on macOS, where
//! `platform::macos::clipboard` reads it, and in tests on every target, so
//! the table is checked on the machine it is edited on.

/// A list of files: one `public.file-url` per pasteboard item.
pub const FILE_URL: &str = "public.file-url";

/// Plain text, which is what `NSPasteboardTypeString` is.
pub const TEXT: &str = "public.utf8-plain-text";

/// The mime the window asks for a list of files by.
pub const URI_LIST: &str = "text/uri-list";

/// The mime text is offered as, the spelling modern toolkits ask for
/// (`crate::clipboard::offer_mimes`).
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// The images that cross in both directions under their own names.
const IMAGES: &[(&str, &str)] = &[
    ("image/png", "public.png"),
    ("image/jpeg", "public.jpeg"),
    ("image/tiff", "public.tiff"),
    ("image/gif", "com.compuserve.gif"),
    ("image/webp", "org.webmproject.webp"),
];

/// The eight bytes every PNG starts with.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A mime without its parameters, as the tables are keyed.
fn base(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
}

/// The type an image copy goes on the pasteboard as.
///
/// An image the table does not name goes as `public.png` only when its
/// bytes are a PNG, whatever the mime said; anything else is `None`, a copy
/// refused rather than bytes announced under a type they are not.
pub fn image_uti(mime: &str, bytes: &[u8]) -> Option<&'static str> {
    let base = base(mime);
    IMAGES
        .iter()
        .find(|(known, _)| *known == base)
        .map(|(_, uti)| *uti)
        .or_else(|| bytes.starts_with(PNG_SIGNATURE).then_some("public.png"))
}

/// What a pasteboard type is called here, if it is one the window can
/// paste.
pub fn mime_of(uti: &str) -> Option<&'static str> {
    match uti {
        FILE_URL => Some(URI_LIST),
        TEXT => Some(TEXT_MIME),
        _ => IMAGES
            .iter()
            .find(|(_, known)| *known == uti)
            .map(|(mime, _)| *mime),
    }
}

/// The pasteboard's types as the window's mimes: each one it can use, in
/// the pasteboard's order, once.
pub fn offered<'a>(types: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for mime in types.into_iter().filter_map(mime_of) {
        if !out.iter().any(|seen| seen == mime) {
            out.push(mime.to_string());
        }
    }
    out
}

/// What to read for a paste the window asked for as `mime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// Every item's file URL, rendered as a `text/uri-list`.
    Files,
    /// The pasteboard's string.
    Text,
    /// The bytes stored under this type.
    Data(&'static str),
}

/// How a paste of `mime` is read, or `None` for a mime the pasteboard has no
/// name for.
pub fn read_for(mime: &str) -> Option<Read> {
    let base = base(mime);
    if base == URI_LIST {
        return Some(Read::Files);
    }
    if base.starts_with("text/") {
        return Some(Read::Text);
    }
    IMAGES
        .iter()
        .find(|(known, _)| *known == base)
        .map(|(_, uti)| Read::Data(uti))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_image_in_the_table_crosses_both_ways_under_one_name() {
        for (mime, uti) in IMAGES {
            assert_eq!(image_uti(mime, b""), Some(*uti), "{mime}");
            assert_eq!(mime_of(uti), Some(*mime), "{uti}");
            assert_eq!(read_for(mime), Some(Read::Data(uti)), "{mime}");
        }
        // Parameters and case are not part of the name.
        assert_eq!(image_uti("Image/PNG; foo=bar", b""), Some("public.png"));
    }

    #[test]
    fn an_unnamed_image_goes_as_png_only_when_it_is_one() {
        let png = [PNG_SIGNATURE, b"rest of the file"].concat();
        assert_eq!(image_uti("image/x-weird", &png), Some("public.png"));
        assert_eq!(image_uti("image/avif", b"\0\0\0 ftypavif"), None);
    }

    #[test]
    fn the_offer_keeps_the_pasteboards_order_and_drops_what_it_cannot_paste() {
        let types = [
            "public.file-url",
            "com.apple.finder.node",
            "public.utf8-plain-text",
            "public.tiff",
            "public.png",
            "public.file-url",
        ];
        assert_eq!(
            offered(types),
            vec![
                "text/uri-list".to_string(),
                "text/plain;charset=utf-8".to_string(),
                "image/tiff".to_string(),
                "image/png".to_string(),
            ]
        );
        assert!(offered(["com.apple.pasteboard.promised-file-url"]).is_empty());
    }

    /// Whatever [`offered`] names, [`read_for`] knows how to read: the
    /// window asks for exactly the mimes it was offered.
    #[test]
    fn every_offered_mime_can_be_read() {
        let everything = [FILE_URL, TEXT]
            .into_iter()
            .chain(IMAGES.iter().map(|(_, uti)| *uti));
        for mime in offered(everything) {
            assert!(read_for(&mime).is_some(), "{mime}");
        }
        assert_eq!(read_for("text/uri-list"), Some(Read::Files));
        assert_eq!(read_for("text/plain"), Some(Read::Text));
        assert_eq!(read_for("application/pdf"), None);
    }
}
