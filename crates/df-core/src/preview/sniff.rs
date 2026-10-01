//! What a file *is*, from its first few kilobytes.
//!
//! [`crate::fs::mime`] guesses from the name because a 200k-entry directory
//! cannot afford an open per row. This is the other half: for the one file
//! under the cursor, the preview pipeline opens it anyway, and once the bytes
//! are in hand the type should be a fact rather than a guess. The two together
//! are the yazi model — a cheap hint everywhere, the truth where it matters.
//!
//! Three reasons the truth matters here, all of them things the hint gets
//! wrong in a way the user sees:
//!
//! - **A lying extension.** `photo.txt` that is a JPEG, `data` with no
//!   extension at all, a `.mp4` that is really a Matroska. Handing a decoder
//!   the wrong container is a failed preview; handing an image decoder a 4 GB
//!   log is worse.
//! - **Extensionless files are the common case** in a source tree: `LICENSE`,
//!   `Makefile`, `.envrc`, a shell script called `deploy`. All of them preview
//!   as text, and only their bytes say so.
//! - **Binary must never reach the text previewer.** A hexdump is the honest
//!   answer for an ELF; three megabytes of mojibake is not.
//!
//! ## Scope
//!
//! Magic numbers for the families PLAN §6 previews or opens, and nothing else.
//! This is not `libmagic` and must not grow into it: every signature here has
//! to earn its place by changing what the preview pane does. The order inside
//! [`sniff`] is deliberate where two signatures overlap — RIFF and ISO-BMFF
//! both need a second look at a later offset before they mean anything.
//!
//! ## The text heuristic
//!
//! Everything that is not a known signature falls to [`looks_like_text`],
//! which is where the defending happens. It is UTF-8 validity over a bounded
//! prefix plus two rules that catch what validity alone lets through — a NUL
//! byte, and a high proportion of control characters. Both exist because
//! `from_utf8` says yes to plenty of binary: a small ELF header is valid
//! UTF-8 more often than not.

use std::path::Path;

use crate::fs::mime::UNKNOWN_MIME;
use crate::{DfError, Result};

/// How much of a file to read before deciding what it is.
///
/// 8 KiB is one or two filesystem blocks — the read costs the same as 512
/// bytes would, and it is enough for every signature here (the deepest,
/// `ustar` in a tar header, sits at offset 257) plus a text sample wide enough
/// that a stray run of control bytes cannot swing the ratio on its own.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// Below this many bytes the control-character ratio is not consulted.
///
/// A ratio over a handful of bytes is noise: `\x1b[31mred\x1b[0m\n` is two
/// escapes in fourteen bytes — 14%, and unmistakably text. 64 bytes is about a
/// line, which is the shortest sample on which "what fraction of this is
/// control characters" means anything. Under it the NUL and UTF-8 gates still
/// apply, and they are the ones that catch actual binary.
pub const MIN_RATIO_SAMPLE: usize = 64;

/// The share of control characters above which a byte run is called binary.
///
/// Real text has a few: newlines and tabs (excluded outright below), the odd
/// form feed, and ANSI escapes in a captured terminal log — which is why the
/// threshold is not zero. It is 5% because a file of ANSI-coloured `ls` output
/// sits around 2–3% and a UTF-8-valid binary blob runs far higher; anything in
/// between is unreadable either way, and a hexdump is the better answer.
pub const CONTROL_RATIO_LIMIT: f32 = 0.05;

/// Byte prefixes that mean exactly one thing.
///
/// Kept as a table so a new format is one line. Signatures needing a second
/// look at another offset (RIFF, ISO-BMFF, EBML, tar) are *not* here — they
/// are handled in [`sniff`] below, where the extra check can be written out.
const MAGIC: &[(&[u8], &str)] = &[
    // Images.
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    (b"\x00\x00\x01\x00", "image/vnd.microsoft.icon"),
    (b"II*\x00", "image/tiff"),
    (b"MM\x00*", "image/tiff"),
    (b"\xff\x0a", "image/jxl"),                       // naked codestream
    (b"\x00\x00\x00\x0cJXL \r\n\x87\n", "image/jxl"), // ISO-BMFF wrapped
    // Audio.
    (b"fLaC", "audio/flac"),
    (b"ID3", "audio/mpeg"),
    // Documents.
    (b"%PDF-", "application/pdf"),
    (b"{\\rtf", "application/rtf"),
    // Affinity's container, whichever app saved it (`preview::affinity`).
    (b"\x00\xffKA", "application/x-affinity"),
    // Archives and compression.
    (b"\x1f\x8b", "application/gzip"),
    (b"\xfd7zXZ\x00", "application/x-xz"),
    (b"\x28\xb5\x2f\xfd", "application/zstd"),
    (b"BZh", "application/x-bzip2"),
    (b"7z\xbc\xaf\x27\x1c", "application/x-7z-compressed"),
    (b"Rar!\x1a\x07", "application/vnd.rar"),
    (b"!<arch>", "application/x-archive"),
    // Executables and other machine formats.
    (b"\x7fELF", "application/x-executable"),
    (b"\x00asm", "application/wasm"),
    (b"SQLite format 3\x00", "application/vnd.sqlite3"),
    // Fonts. The old Mac `true` flavour is deliberately absent: a two-word
    // signature that spells an English word would type every JSON file
    // containing nothing but `true` as a typeface.
    (b"\x00\x01\x00\x00\x00", "font/ttf"),
    (b"OTTO", "font/otf"),
    (b"ttcf", "font/collection"),
    (b"wOFF", "font/woff"),
    (b"wOF2", "font/woff2"),
];

/// ISO-BMFF brands at offset 8, longest-lived first. The `ftyp` box names the
/// standard a file claims to follow, which is the only thing separating an
/// AVIF still, an HEIC burst, an iPhone video and an m4a — all four are the
/// same container.
const FTYP_BRANDS: &[(&[u8], &str)] = &[
    (b"avif", "image/avif"),
    (b"avis", "image/avif"),
    (b"heic", "image/heic"),
    (b"heix", "image/heic"),
    (b"heim", "image/heic"),
    (b"heis", "image/heic"),
    (b"hevc", "image/heic"),
    (b"mif1", "image/heif"),
    (b"msf1", "image/heif"),
    (b"M4A ", "audio/mp4"),
    (b"M4B ", "audio/mp4"),
    (b"qt  ", "video/quicktime"),
    (b"M4V ", "video/x-m4v"),
    (b"3gp", "video/3gpp"),
];

/// The content type of `head`, from magic bytes alone.
///
/// `None` means "no signature matched", not "unknown" — the caller decides
/// what to do with that, usually by asking [`looks_like_text`] and then
/// falling back to the name hint.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    // RIFF and ISO-BMFF first: their leading bytes are shared by several
    // formats, so a table lookup on the prefix alone would answer too early.
    if let Some(mime) = sniff_riff(head) {
        return Some(mime);
    }
    if let Some(mime) = sniff_ftyp(head) {
        return Some(mime);
    }
    if let Some(mime) = sniff_ebml(head) {
        return Some(mime);
    }
    if let Some(mime) = sniff_ogg(head) {
        return Some(mime);
    }
    if let Some(mime) = sniff_zip(head) {
        return Some(mime);
    }
    if starts_with(head, 257, b"ustar") {
        return Some("application/x-tar");
    }
    // BMP's signature is two ASCII letters, which is how plenty of prose
    // starts ("BMW", "BM 25"). The four reserved bytes at offset 6 are zero in
    // every real BMP, and checking them is what makes the claim safe.
    if head.starts_with(b"BM") && head.len() >= 14 && head[6..10] == [0, 0, 0, 0] {
        return Some("image/bmp");
    }

    for (magic, mime) in MAGIC {
        if head.starts_with(magic) {
            return Some(mime);
        }
    }

    // An MPEG audio frame with no ID3 tag: 11 sync bits, and the next four
    // must not be the reserved layer/version combinations, or every second
    // binary file would be "an mp3".
    if head.len() >= 2 && head[0] == 0xff && (head[1] & 0xe0) == 0xe0 {
        let version = (head[1] >> 3) & 0b11;
        let layer = (head[1] >> 1) & 0b11;
        if version != 0b01 && layer != 0b00 {
            return Some("audio/mpeg");
        }
    }

    None
}

/// RIFF: four bytes of container, four of length, four of what it holds.
fn sniff_riff(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"RIFF") || head.len() < 12 {
        return None;
    }
    match &head[8..12] {
        b"WEBP" => Some("image/webp"),
        b"WAVE" => Some("audio/wav"),
        b"AVI " => Some("video/x-msvideo"),
        _ => None,
    }
}

/// ISO base media: `[u32 size]["ftyp"][brand]`. mp4, mov, avif, heic and m4a
/// are one container wearing different brands.
fn sniff_ftyp(head: &[u8]) -> Option<&'static str> {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return None;
    }
    let brand = &head[8..12];
    for (b, mime) in FTYP_BRANDS {
        if brand.starts_with(b) {
            return Some(mime);
        }
    }
    // isom, iso2, mp41, mp42, dash and the rest of the plain-MP4 crowd. The
    // default is video rather than `None` because an unknown brand on this
    // container is still something a video decoder can open.
    Some("video/mp4")
}

/// EBML: Matroska and WebM share a header and differ only by DocType, which
/// sits a few bytes in as a plain ASCII string.
fn sniff_ebml(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"\x1a\x45\xdf\xa3") {
        return None;
    }
    let window = &head[..head.len().min(64)];
    if contains(window, b"webm") {
        return Some("video/webm");
    }
    Some("video/x-matroska")
}

/// Ogg carries several codecs; the codec's own header follows the page header.
fn sniff_ogg(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"OggS") {
        return None;
    }
    let window = &head[..head.len().min(128)];
    if contains(window, b"OpusHead") {
        return Some("audio/opus");
    }
    if contains(window, b"\x80theora") {
        return Some("video/ogg");
    }
    // Vorbis, FLAC-in-Ogg and Speex all preview as audio, so they need no
    // further separation here.
    Some("audio/ogg")
}

/// Zip, and the two formats that are a zip with a fixed first member. epub is
/// worth separating because it previews; docx and friends are not, since they
/// go to an opener either way.
fn sniff_zip(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"PK\x03\x04")
        && !head.starts_with(b"PK\x05\x06")
        && !head.starts_with(b"PK\x07\x08")
    {
        return None;
    }
    if starts_with(head, 30, b"mimetypeapplication/epub+zip") {
        return Some("application/epub+zip");
    }
    Some("application/zip")
}

fn starts_with(head: &[u8], offset: usize, needle: &[u8]) -> bool {
    head.len() >= offset + needle.len() && &head[offset..offset + needle.len()] == needle
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Whether `head` reads as text a human wrote.
///
/// Three gates, cheapest first:
///
/// 1. **No NUL.** One `\0` and it is binary, full stop. UTF-16 is the famous
///    exception and is handled by its BOM above the gate — a Windows text file
///    stays text, and everything else with an embedded NUL is a hexdump.
/// 2. **Valid UTF-8**, allowing for the multi-byte sequence the 8 KiB cut may
///    have sliced in half. Refusing that would call every long UTF-8 file
///    binary one time in four.
/// 3. **Few control characters.** Tab, newline, carriage return and form feed
///    do not count; everything else under 0x20, plus DEL, does. See
///    [`CONTROL_RATIO_LIMIT`].
///
/// An empty slice is text: an empty file previews as an empty document, not as
/// an empty hexdump.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return true;
    }
    // A BOM is a declaration, and UTF-16's is full of NULs that gate 1 would
    // otherwise reject.
    if head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff]) {
        return true;
    }
    if head.contains(&0) {
        return false;
    }

    let sample = &head[..head.len().min(SNIFF_BYTES)];
    let valid = match std::str::from_utf8(sample) {
        Ok(_) => sample.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => return false,
    };
    // A cut that leaves almost nothing valid is not a cut, it is binary that
    // happened to start with an ASCII byte.
    if valid == 0 {
        return false;
    }

    if valid < MIN_RATIO_SAMPLE {
        return true;
    }

    let control = sample[..valid]
        .iter()
        .filter(|b| (**b < 0x20 && !matches!(**b, b'\t' | b'\n' | b'\r' | 0x0c)) || **b == 0x7f)
        .count();
    (control as f32) < CONTROL_RATIO_LIMIT * valid as f32
}

/// The type of `head`, with the name's guess as the tiebreaker.
///
/// The precedence is the whole point, so it is written out:
///
/// 1. **A signature wins.** Bytes do not lie about what they are, and a
///    `.txt` holding a JPEG is a JPEG.
/// 2. **Otherwise, if it reads as text**, keep the hint when the hint is
///    itself textual — that is how `main.rs` stays `text/rust` and gets syntax
///    highlighting instead of collapsing to `text/plain`. `.obj`, `.ply` and
///    `.gcode` are the shape of this rule too: text files whose *hint* is the
///    only thing that knows they get a turntable rather than a scrollback.
/// 3. **Otherwise it is binary**, and a hint that names a binary format is
///    still better than nothing: `.wmv` and `.docx` have no signature here but
///    the opener rules match on them.
pub fn sniff_or_hint(head: &[u8], hint: &'static str) -> &'static str {
    if let Some(mime) = sniff(head) {
        return mime;
    }
    let hint_is_text = crate::fs::mime::is_text(hint) || TEXTUAL_HINTS.contains(&hint);
    if looks_like_text(head) {
        if hint_is_text {
            return hint;
        }
        // A shebang is a stronger claim than an unrecognised name.
        if head.starts_with(b"#!") {
            return "application/x-shellscript";
        }
        return "text/plain";
    }
    if hint != UNKNOWN_MIME && !hint_is_text {
        return hint;
    }
    UNKNOWN_MIME
}

/// Mimes that are text on disk but are not `text/*` and are not in
/// [`crate::fs::mime::is_text`]'s list, because what they *mean* is not text.
/// Keeping the hint for these is what stops an `.obj` model or an `.svg` from
/// previewing as a wall of source.
const TEXTUAL_HINTS: &[&str] = &[
    "image/svg+xml",
    "model/obj",
    "model/ply",
    "model/stl", // the ASCII flavour; the binary one has no signature either
];

/// Read the head of a file and type it. The one io call in this module.
///
/// A directory, a device or an unreadable file is the caller's problem — this
/// reports the io error with its path rather than silently answering
/// "unknown", because the preview pane needs to tell "empty" from "denied".
pub fn sniff_file(path: &Path, hint: &'static str) -> Result<&'static str> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| DfError::io(path, e))?;
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut filled = 0;
    // One `read` can come back short for reasons that are not EOF (a signal, a
    // pipe), and a short read would move the tar magic out from under its
    // offset. Loop until full or done.
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DfError::io(path, e)),
        }
    }
    head.truncate(filled);
    Ok(sniff_or_hint(&head, hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handcrafted header: the magic, then padding, so the length checks in
    /// the offset-based signatures have something to look at.
    fn fixture(magic: &[u8]) -> Vec<u8> {
        let mut v = magic.to_vec();
        v.resize(magic.len().max(64), b'\0');
        v
    }

    #[test]
    fn magic_bytes_name_their_format() {
        let cases: &[(&[u8], &str)] = &[
            (b"\x89PNG\r\n\x1a\n", "image/png"),
            (b"\xff\xd8\xff\xe0", "image/jpeg"),
            (b"GIF89a", "image/gif"),
            (b"%PDF-1.7", "application/pdf"),
            (b"\x7fELF\x02\x01\x01", "application/x-executable"),
            (b"SQLite format 3\x00", "application/vnd.sqlite3"),
            (b"\x1f\x8b\x08", "application/gzip"),
            (b"\xfd7zXZ\x00", "application/x-xz"),
            (b"\x28\xb5\x2f\xfd", "application/zstd"),
            (b"BZh9", "application/x-bzip2"),
            (b"7z\xbc\xaf\x27\x1c", "application/x-7z-compressed"),
            (b"Rar!\x1a\x07\x01\x00", "application/vnd.rar"),
            (b"fLaC\x00", "audio/flac"),
            (b"ID3\x03", "audio/mpeg"),
            (b"\xff\xfb\x90\x44", "audio/mpeg"),
            (b"OTTO\x00", "font/otf"),
            (b"wOF2\x00", "font/woff2"),
        ];
        for (bytes, expected) in cases {
            let head = fixture(bytes);
            assert_eq!(sniff(&head), Some(*expected), "for {bytes:x?}");
        }
    }

    #[test]
    fn riff_needs_its_second_word() {
        assert_eq!(
            sniff(&fixture(b"RIFF\x00\x00\x00\x00WEBP")),
            Some("image/webp")
        );
        assert_eq!(
            sniff(&fixture(b"RIFF\x00\x00\x00\x00WAVE")),
            Some("audio/wav")
        );
        assert_eq!(
            sniff(&fixture(b"RIFF\x00\x00\x00\x00AVI ")),
            Some("video/x-msvideo")
        );
        // RIFF alone says nothing, and must not be guessed at.
        assert_eq!(sniff(&fixture(b"RIFF\x00\x00\x00\x00NOPE")), None);
    }

    #[test]
    fn one_container_four_brands() {
        assert_eq!(
            sniff(&fixture(b"\x00\x00\x00\x20ftypavif")),
            Some("image/avif")
        );
        assert_eq!(
            sniff(&fixture(b"\x00\x00\x00\x20ftypheic")),
            Some("image/heic")
        );
        assert_eq!(
            sniff(&fixture(b"\x00\x00\x00\x20ftypM4A ")),
            Some("audio/mp4")
        );
        assert_eq!(
            sniff(&fixture(b"\x00\x00\x00\x20ftypqt  ")),
            Some("video/quicktime")
        );
        // An unknown brand is still an mp4-shaped file.
        assert_eq!(
            sniff(&fixture(b"\x00\x00\x00\x20ftypisom")),
            Some("video/mp4")
        );
    }

    #[test]
    fn matroska_and_webm_differ_only_by_doctype() {
        let mut mkv = fixture(b"\x1a\x45\xdf\xa3\x01\x00\x00\x00");
        mkv.splice(20..28, *b"matroska");
        assert_eq!(sniff(&mkv), Some("video/x-matroska"));

        let mut webm = fixture(b"\x1a\x45\xdf\xa3\x01\x00\x00\x00");
        webm.splice(20..24, *b"webm");
        assert_eq!(sniff(&webm), Some("video/webm"));
    }

    #[test]
    fn ogg_reports_its_codec() {
        let mut opus = fixture(b"OggS\x00\x02");
        opus.splice(28..36, *b"OpusHead");
        assert_eq!(sniff(&opus), Some("audio/opus"));
        assert_eq!(sniff(&fixture(b"OggS\x00\x02")), Some("audio/ogg"));
    }

    #[test]
    fn tar_magic_lives_at_offset_257() {
        let mut tar = vec![b'x'; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&tar), Some("application/x-tar"));
        // A truncated header must not be read past the end.
        assert_eq!(sniff(&tar[..100]), None);
    }

    #[test]
    fn epub_is_a_zip_that_says_so() {
        let mut epub = vec![0u8; 128];
        epub[..4].copy_from_slice(b"PK\x03\x04");
        epub[30..58].copy_from_slice(b"mimetypeapplication/epub+zip");
        assert_eq!(sniff(&epub), Some("application/epub+zip"));

        let mut zip = vec![0u8; 128];
        zip[..4].copy_from_slice(b"PK\x03\x04");
        assert_eq!(sniff(&zip), Some("application/zip"));
    }

    #[test]
    fn bmp_is_only_claimed_when_its_reserved_bytes_are_zero() {
        let mut bmp = vec![0u8; 64];
        bmp[..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(sniff(&bmp), Some("image/bmp"));
        // Prose that happens to start with the letters.
        assert_eq!(sniff(b"BMW ownership is a lifestyle\n"), None);
    }

    #[test]
    fn plain_text_has_no_signature() {
        assert_eq!(sniff(b"hello, world\n"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn text_and_binary_are_told_apart() {
        assert!(looks_like_text(b""), "empty is an empty document");
        assert!(looks_like_text(b"fn main() {\n    println!(\"hi\");\n}\n"));
        assert!(
            looks_like_text("café — naïve\n".as_bytes()),
            "utf-8 is text"
        );
        assert!(
            looks_like_text(b"\x1b[31mred\x1b[0m\n"),
            "a few escapes are fine"
        );
        assert!(
            looks_like_text(&[0xff, 0xfe, b'h', 0, b'i', 0]),
            "utf-16 bom"
        );

        assert!(!looks_like_text(b"binary\x00with a nul"));
        assert!(!looks_like_text(
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08].repeat(16)
        ));
        assert!(!looks_like_text(&[0xc3, 0x28, 0xc3, 0x28]), "invalid utf-8");
    }

    #[test]
    fn a_truncated_multibyte_tail_is_still_text() {
        // 8 KiB of prose cut mid-character — the common case for any file
        // longer than the sniff window.
        let mut bytes = "é".repeat(100).into_bytes();
        bytes.pop();
        assert!(looks_like_text(&bytes));
    }

    #[test]
    fn a_sample_too_short_to_have_a_ratio_is_not_judged_by_one() {
        // Two escapes in fourteen bytes is 14% and obviously text.
        assert!(looks_like_text(b"\x1b[31mred\x1b[0m\n"));
        // The same density over a real sample is not.
        assert!(!looks_like_text(&b"\x1b[31mred\x1b[0m\n".repeat(8)));
    }

    #[test]
    fn the_control_ratio_defends_the_text_previewer() {
        // Just under the limit: 4 escapes in 100 bytes.
        let mut ok = vec![b'a'; 96];
        ok.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        assert!(looks_like_text(&ok));

        // Well over it.
        let mut bad = vec![b'a'; 80];
        bad.extend(std::iter::repeat_n(0x01, 20));
        assert!(!looks_like_text(&bad));
    }

    #[test]
    fn bytes_beat_the_name() {
        // A JPEG called `.txt` is a JPEG.
        let jpeg = fixture(b"\xff\xd8\xff\xe0");
        assert_eq!(sniff_or_hint(&jpeg, "text/plain"), "image/jpeg");
    }

    #[test]
    fn a_textual_hint_survives_so_syntax_does() {
        let source = b"fn main() {}\n";
        assert_eq!(sniff_or_hint(source, "text/rust"), "text/rust");
        assert_eq!(
            sniff_or_hint(source, "application/json"),
            "application/json"
        );
        assert_eq!(sniff_or_hint(b"o 1\nv 0 0 0\n", "model/obj"), "model/obj");
        // A name that says nothing about a text file: plain text.
        assert_eq!(sniff_or_hint(source, UNKNOWN_MIME), "text/plain");
        assert_eq!(
            sniff_or_hint(b"#!/bin/sh\necho hi\n", UNKNOWN_MIME),
            "application/x-shellscript"
        );
    }

    #[test]
    fn an_unsigned_binary_keeps_its_binary_hint() {
        let blob = [0u8, 1, 2, 3, 4, 5, 6, 7];
        assert_eq!(sniff_or_hint(&blob, "video/x-ms-wmv"), "video/x-ms-wmv");
        assert_eq!(sniff_or_hint(&blob, "text/plain"), UNKNOWN_MIME);
        assert_eq!(sniff_or_hint(&blob, UNKNOWN_MIME), UNKNOWN_MIME);
    }

    #[test]
    fn sniffing_a_real_file_reads_only_its_head() {
        let dir = std::env::temp_dir().join(format!("df-sniff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("big.png");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(SNIFF_BYTES * 4, 0x55);
        std::fs::write(&path, &bytes).expect("write fixture");

        assert_eq!(sniff_file(&path, "text/plain").expect("sniff"), "image/png");
        let _ = std::fs::remove_file(&path);

        let missing = dir.join("gone");
        assert!(sniff_file(&missing, "text/plain").is_err());
    }
}
