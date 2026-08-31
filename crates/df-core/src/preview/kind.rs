//! Which previewer a file gets.
//!
//! One function, one table, one enum. PLAN §6 lists the built-in previewers —
//! text, markdown, image, video, audio, PDF, font, 3D model, G-code, directory
//! listing, archive contents — and this decides which of them a file belongs
//! to, from its entry and its sniffed type ([`super::sniff`]).
//!
//! It is deliberately a **pure function of two values**. Everything expensive
//! has already happened by the time it is called (the stat at scan time, the
//! 8 KiB read at sniff time), so this is a table lookup that cannot block, and
//! the whole matrix of file kinds is a unit test rather than an afternoon of
//! clicking through directories.
//!
//! ## The three answers that are not previewers
//!
//! - [`PreviewKind::Binary`] — no previewer, but a hexdump is a real, useful
//!   answer for an ELF or a `.pyc`, and PLAN §6's "never a failure" rule means
//!   showing *something* beats an error.
//! - [`PreviewKind::Unsupported`] — there is nothing honest to draw. Office
//!   documents are the case PLAN §6 explicitly defers ("Office docs: defer;
//!   keep the opener path"), and devices and sockets join them.
//! - [`PreviewKind::Denied`] — the file exists and cannot be read. Distinct
//!   from unsupported because the user can *act* on it, and the pane should
//!   say "permission denied" rather than shrugging.

use crate::fs::mime::{is_text, BROKEN_LINK_MIME, DIR_MIME};
use crate::fs::{Entry, Kind, LinkTarget};

use super::syntax::syntax_for_name;

/// What the preview pane should draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewKind {
    /// A directory: a one-level listing (PLAN §6, "replaces piper/eza").
    Directory,
    /// Zero bytes. Its own answer because "nothing to show" and "we could not
    /// read it" look identical on screen and must not.
    Empty,
    /// Text, with the language name the highlighter wants (`None` = prose).
    Text {
        syntax: Option<&'static str>,
    },
    /// Markdown, which is text that gets rendered rather than highlighted.
    Markdown,
    Image,
    Video,
    Audio,
    Pdf,
    /// A typeface, for the specimen sheet.
    Font,
    /// stl/obj/ply/3mf — the turntable.
    Model3d,
    /// A toolpath, for the layer view.
    Gcode,
    /// zip/tar/7z/rar and the compressed singletons: a listing of contents.
    Archive,
    /// Bytes with no previewer, shown as a hexdump.
    Binary,
    /// Nothing to draw; the opener rules are the whole answer. Office
    /// documents, sockets, fifos, devices, broken links.
    Unsupported,
    /// Readable in principle, not by us.
    Denied,
}

impl PreviewKind {
    /// Whether df-app has to hand this to a decoder (dv-media, pdfium,
    /// ttf-parser) rather than rendering it from what df-core already read.
    ///
    /// This is the seam. df-core will not link a decoder — PLAN §1 keeps the
    /// headless half testable without ffmpeg — so for these kinds the worker
    /// answers [`super::job::Preview::NeedsDecode`] and df-app takes over.
    pub fn needs_decode(&self) -> bool {
        matches!(
            self,
            PreviewKind::Image
                | PreviewKind::Video
                | PreviewKind::Audio
                | PreviewKind::Pdf
                | PreviewKind::Font
                | PreviewKind::Model3d
        )
    }

    /// Whether a yazi-cached thumbnail could stand in while the decode runs
    /// (PLAN §6's crossfade). Only the kinds yazi itself thumbnails.
    pub fn thumbnailable(&self) -> bool {
        matches!(
            self,
            PreviewKind::Image | PreviewKind::Video | PreviewKind::Pdf
        )
    }
}

/// Exact mime → previewer, for the types whose family prefix would get them
/// wrong. Checked before [`FAMILIES`], so this is where every exception lives.
const EXACT: &[(&str, PreviewKind)] = &[
    // Text that is not prose.
    ("text/markdown", PreviewKind::Markdown),
    ("text/x.gcode", PreviewKind::Gcode),
    // ASCII 3D formats: `model/*` by hint, plain text by sniffing, and a
    // turntable either way.
    ("model/obj", PreviewKind::Model3d),
    ("model/ply", PreviewKind::Model3d),
    ("model/stl", PreviewKind::Model3d),
    ("model/3mf", PreviewKind::Model3d),
    // SVG is text on disk and a picture on screen. It goes to the image path
    // because that is what the user means by it.
    ("image/svg+xml", PreviewKind::Image),
    ("application/pdf", PreviewKind::Pdf),
    // Archives, including the compressed singletons that are not really
    // archives — `.gz` of one file still lists as its contents.
    ("application/zip", PreviewKind::Archive),
    ("application/gzip", PreviewKind::Archive),
    ("application/zstd", PreviewKind::Archive),
    ("application/x-tar", PreviewKind::Archive),
    ("application/x-xz", PreviewKind::Archive),
    ("application/x-bzip2", PreviewKind::Archive),
    ("application/x-7z-compressed", PreviewKind::Archive),
    ("application/vnd.rar", PreviewKind::Archive),
    ("application/x-archive", PreviewKind::Archive),
    ("application/vnd.comicbook+zip", PreviewKind::Archive),
    ("application/vnd.comicbook-rar", PreviewKind::Archive),
    (
        "application/vnd.debian.binary-package",
        PreviewKind::Archive,
    ),
    ("application/x-iso9660-image", PreviewKind::Archive),
    // Office and e-books: PLAN §6 defers these to the opener path. epub is
    // here rather than under Archive on purpose — listing its zip members is
    // a worse answer than the opener, not a better one.
    ("application/epub+zip", PreviewKind::Unsupported),
    ("application/msword", PreviewKind::Unsupported),
    ("application/rtf", PreviewKind::Unsupported),
    ("application/vnd.ms-excel", PreviewKind::Unsupported),
    ("application/vnd.ms-powerpoint", PreviewKind::Unsupported),
];

/// mime prefix → previewer, longest match first. The bulk of the decision:
/// everything in a family previews the same way.
const FAMILIES: &[(&str, PreviewKind)] = &[
    ("image/", PreviewKind::Image),
    ("video/", PreviewKind::Video),
    ("audio/", PreviewKind::Audio),
    ("font/", PreviewKind::Font),
    ("model/", PreviewKind::Model3d),
    // Anything `application/vnd.openxmlformats-` or `.oasis.` — the office
    // families, matched by prefix so a new member of either needs no entry.
    ("application/vnd.openxmlformats-", PreviewKind::Unsupported),
    ("application/vnd.oasis.", PreviewKind::Unsupported),
];

/// Which previewer `entry` gets, given the type [`super::sniff`] settled on.
///
/// The order of the checks is the specification:
///
/// 1. **What it is on the filesystem** — a directory lists, a device or a
///    broken link has nothing to show. This outranks the mime because a fifo
///    named `photo.png` must never be opened by an image decoder that will
///    then block forever on it.
/// 2. **Empty** — before the type, because zero bytes cannot be previewed as
///    anything and every decoder would fail on them.
/// 3. **The type** — exact table, then family prefix, then "is it text?".
/// 4. **Binary** — the hexdump fallback, which always answers.
pub fn kind_for(entry: &Entry, mime: &str) -> PreviewKind {
    if entry.is_dir() || mime == DIR_MIME {
        return PreviewKind::Directory;
    }
    match entry.kind {
        Kind::Symlink { target: None } => return PreviewKind::Unsupported,
        Kind::Symlink {
            target: Some(LinkTarget::Other),
        } => return PreviewKind::Unsupported,
        _ => {}
    }
    if mime == BROKEN_LINK_MIME {
        return PreviewKind::Unsupported;
    }
    if entry.len == 0 {
        return PreviewKind::Empty;
    }

    kind_for_mime(mime, &entry.name)
}

/// The type half of [`kind_for`], split out so the table can be tested without
/// building an [`Entry`] for every row.
pub fn kind_for_mime(mime: &str, name: &str) -> PreviewKind {
    if let Some((_, kind)) = EXACT.iter().find(|(m, _)| *m == mime) {
        return kind.clone();
    }
    if let Some((_, kind)) = FAMILIES.iter().find(|(prefix, _)| mime.starts_with(prefix)) {
        return kind.clone();
    }
    if is_text(mime) {
        return PreviewKind::Text {
            syntax: syntax_for_name(name),
        };
    }
    PreviewKind::Binary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: Kind, len: u64, mime: &'static str) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            kind,
            len,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            is_hidden: name.starts_with('.'),
            mime,
        }
    }

    fn file(name: &str, len: u64) -> Entry {
        entry(name, Kind::File, len, "text/plain")
    }

    #[test]
    fn the_type_table_covers_every_previewer() {
        let cases: &[(&str, &str, PreviewKind)] = &[
            ("photo.png", "image/png", PreviewKind::Image),
            ("photo.avif", "image/avif", PreviewKind::Image),
            ("logo.svg", "image/svg+xml", PreviewKind::Image),
            ("clip.mp4", "video/mp4", PreviewKind::Video),
            ("clip.mkv", "video/x-matroska", PreviewKind::Video),
            ("song.flac", "audio/flac", PreviewKind::Audio),
            ("song.opus", "audio/opus", PreviewKind::Audio),
            ("book.pdf", "application/pdf", PreviewKind::Pdf),
            ("face.ttf", "font/ttf", PreviewKind::Font),
            ("part.stl", "model/stl", PreviewKind::Model3d),
            ("part.3mf", "model/3mf", PreviewKind::Model3d),
            ("part.gcode", "text/x.gcode", PreviewKind::Gcode),
            ("notes.md", "text/markdown", PreviewKind::Markdown),
            ("src.tar.gz", "application/gzip", PreviewKind::Archive),
            ("src.zip", "application/zip", PreviewKind::Archive),
            (
                "src.7z",
                "application/x-7z-compressed",
                PreviewKind::Archive,
            ),
            ("a.out", "application/x-executable", PreviewKind::Binary),
            ("db.sqlite", "application/vnd.sqlite3", PreviewKind::Binary),
            ("blob", "application/octet-stream", PreviewKind::Binary),
        ];
        for (name, mime, expected) in cases {
            assert_eq!(kind_for_mime(mime, name), *expected, "for {name} ({mime})");
        }
    }

    #[test]
    fn text_carries_its_language_through() {
        assert_eq!(
            kind_for_mime("text/rust", "main.rs"),
            PreviewKind::Text {
                syntax: Some("Rust")
            }
        );
        assert_eq!(
            kind_for_mime("application/json", "package.json"),
            PreviewKind::Text {
                syntax: Some("JSON")
            }
        );
        assert_eq!(
            kind_for_mime("text/plain", "LICENSE"),
            PreviewKind::Text { syntax: None }
        );
    }

    #[test]
    fn office_documents_go_to_the_opener_not_a_previewer() {
        let cases = [
            "application/msword",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.oasis.opendocument.text",
            "application/epub+zip",
        ];
        for mime in cases {
            assert_eq!(
                kind_for_mime(mime, "doc"),
                PreviewKind::Unsupported,
                "for {mime}"
            );
        }
    }

    #[test]
    fn what_it_is_outranks_what_it_is_called() {
        // A fifo named like an image. Opening it with a decoder would block
        // forever, which is the bug this ordering exists to prevent.
        let fifo = entry(
            "photo.png",
            Kind::Symlink {
                target: Some(LinkTarget::Other),
            },
            1024,
            "image/png",
        );
        assert_eq!(kind_for(&fifo, "image/png"), PreviewKind::Unsupported);

        let broken = entry(
            "gone.png",
            Kind::Symlink { target: None },
            0,
            "inode/symlink",
        );
        assert_eq!(kind_for(&broken, "image/png"), PreviewKind::Unsupported);

        let dir = entry("src", Kind::Dir, 0, DIR_MIME);
        assert_eq!(kind_for(&dir, DIR_MIME), PreviewKind::Directory);

        // A symlink to a directory lists like one.
        let link = entry(
            "link",
            Kind::Symlink {
                target: Some(LinkTarget::Dir),
            },
            0,
            DIR_MIME,
        );
        assert_eq!(kind_for(&link, DIR_MIME), PreviewKind::Directory);
    }

    #[test]
    fn empty_is_its_own_answer() {
        assert_eq!(
            kind_for(&file("empty.png", 0), "image/png"),
            PreviewKind::Empty
        );
        assert_eq!(
            kind_for(&file("empty.rs", 0), "text/rust"),
            PreviewKind::Empty
        );
        // One byte is not empty.
        assert_eq!(
            kind_for(&file("one.rs", 1), "text/rust"),
            PreviewKind::Text {
                syntax: Some("Rust")
            }
        );
    }

    #[test]
    fn the_seam_flags_agree_with_the_variants() {
        assert!(PreviewKind::Image.needs_decode());
        assert!(PreviewKind::Video.needs_decode());
        assert!(PreviewKind::Pdf.needs_decode());
        assert!(PreviewKind::Font.needs_decode());
        assert!(!PreviewKind::Text { syntax: None }.needs_decode());
        assert!(!PreviewKind::Directory.needs_decode());
        assert!(!PreviewKind::Archive.needs_decode());

        assert!(PreviewKind::Image.thumbnailable());
        assert!(PreviewKind::Video.thumbnailable());
        assert!(!PreviewKind::Audio.thumbnailable());
        assert!(!PreviewKind::Binary.thumbnailable());
    }

    #[test]
    fn every_exact_entry_is_unique_and_not_shadowed_by_a_family() {
        let mut seen = std::collections::HashSet::new();
        for (mime, _) in EXACT {
            assert!(seen.insert(*mime), "{mime} appears twice");
        }
        // The exact table is checked first, so a family prefix that also
        // matches is fine — but a *family* entry that duplicates another
        // family's prefix would be dead code.
        for (i, (a, _)) in FAMILIES.iter().enumerate() {
            for (b, _) in &FAMILIES[..i] {
                assert!(!a.starts_with(b), "{a} is shadowed by {b}");
            }
        }
    }
}
