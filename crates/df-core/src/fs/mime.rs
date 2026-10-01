//! A mime *hint*, from the file name alone.
//!
//! PLAN §6 gives the preview pipeline real type detection later — sniffing
//! magic bytes costs an open and a read per file, which is exactly the thing a
//! directory of 200k entries cannot afford while the list is still painting.
//! What the list actually needs from a type is small: which icon to draw, which
//! opener rule matches ([`crate::config::Config::openers_for`]), and whether
//! the preview pane should even bother. An extension answers all three for
//! everything a human names, and is a table lookup.
//!
//! So this is deliberately a hint and is named one. When the sniffer lands it
//! overwrites the hint on the entries that are actually on screen; every other
//! entry keeps the guess, which is right often enough to draw with.

/// Directories. `inode/directory` is what `file(1)`, xdg and yazi all say, and
/// [`crate::config`]'s `*/` opener rule leans on the same convention.
pub const DIR_MIME: &str = "inode/directory";

/// A symlink whose target does not exist. Nothing can open it, and the list
/// wants to draw it differently, so it gets its own type rather than being
/// reported as whatever its name suggests.
pub const BROKEN_LINK_MIME: &str = "inode/symlink";

/// Anything unrecognised. Not a guess — an admission, and the fallback opener
/// rule (`*`) is written to catch it.
pub const UNKNOWN_MIME: &str = "application/octet-stream";

/// extension → mime, lowercase, sorted by family.
///
/// Scope: every extension the opener rules in [`crate::config`] match on, plus
/// the ones a preview exists for (PLAN §6). Adding an extension here is how a
/// file gets an icon and an opener; there is no other table.
const EXTENSIONS: &[(&str, &str)] = &[
    // Images, including the ones the AVIF/HEIF plugin existed for.
    ("apng", "image/apng"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("gif", "image/gif"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("ico", "image/vnd.microsoft.icon"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("jxl", "image/jxl"),
    ("png", "image/png"),
    ("svg", "image/svg+xml"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("webp", "image/webp"),
    // Video.
    ("avi", "video/x-msvideo"),
    ("m4v", "video/x-m4v"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp4", "video/mp4"),
    ("mpeg", "video/mpeg"),
    ("mpg", "video/mpeg"),
    ("ts", "video/mp2t"), // also TypeScript; see `disambiguate` below
    ("webm", "video/webm"),
    ("wmv", "video/x-ms-wmv"),
    // Audio.
    ("aac", "audio/aac"),
    ("aiff", "audio/aiff"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("mp3", "audio/mpeg"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/opus"),
    ("wav", "audio/wav"),
    ("wma", "audio/x-ms-wma"),
    // Documents.
    // Affinity's, whose one container every app saves (preview::affinity).
    ("af", "application/x-affinity"),
    ("afdesign", "application/x-affinity"),
    ("afphoto", "application/x-affinity"),
    ("afpub", "application/x-affinity"),
    ("aftemplate", "application/x-affinity"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("epub", "application/epub+zip"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("pdf", "application/pdf"),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("rtf", "application/rtf"),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    // Archives — the family PLAN §6 says needs an extract rule.
    ("7z", "application/x-7z-compressed"),
    ("bz2", "application/x-bzip2"),
    ("cbr", "application/vnd.comicbook-rar"),
    ("cbz", "application/vnd.comicbook+zip"),
    ("gz", "application/gzip"),
    ("rar", "application/vnd.rar"),
    ("tar", "application/x-tar"),
    ("tgz", "application/gzip"),
    ("xz", "application/x-xz"),
    ("zip", "application/zip"),
    ("zst", "application/zstd"),
    // Fonts (specimen previews).
    ("otf", "font/otf"),
    ("ttc", "font/collection"),
    ("ttf", "font/ttf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    // 3D and toolpaths. `.obj` and `.ply` really are text/plain to every
    // sniffer on the machine, which is exactly why the opener rules match them
    // by glob — but the *hint* can be honest, and the preview pane uses it.
    ("3mf", "model/3mf"),
    ("gcode", "text/x.gcode"),
    ("gco", "text/x.gcode"),
    ("obj", "model/obj"),
    ("ply", "model/ply"),
    ("stl", "model/stl"),
    // Structured text that has its own type.
    ("csv", "text/csv"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("json", "application/json"),
    ("jsonl", "application/ndjson"),
    ("md", "text/markdown"),
    ("ndjson", "application/ndjson"),
    ("toml", "application/toml"),
    ("xml", "application/xml"),
    ("yaml", "application/x-yaml"),
    ("yml", "application/x-yaml"),
    // Source and plain text. Everything here previews with syntax highlighting,
    // opens in $EDITOR, and must never be handed to an image decoder.
    ("c", "text/x-c"),
    ("cc", "text/x-c++"),
    ("cfg", "text/plain"),
    ("conf", "text/plain"),
    ("cpp", "text/x-c++"),
    ("cs", "text/x-csharp"),
    ("css", "text/css"),
    ("go", "text/x-go"),
    ("h", "text/x-c"),
    ("hpp", "text/x-c++"),
    ("ini", "text/plain"),
    ("java", "text/x-java"),
    ("js", "text/javascript"),
    ("jsx", "text/javascript"),
    ("log", "text/plain"),
    ("lua", "text/x-lua"),
    ("mjs", "text/javascript"),
    ("patch", "text/x-diff"),
    ("php", "text/x-php"),
    ("py", "text/x-python"),
    ("rb", "text/x-ruby"),
    ("rs", "text/rust"),
    ("sh", "application/x-shellscript"),
    ("sql", "text/x-sql"),
    ("svelte", "text/html"),
    ("tsx", "text/typescript"),
    ("txt", "text/plain"),
    ("vue", "text/html"),
    ("zsh", "application/x-shellscript"),
    // Executables and libraries.
    ("a", "application/x-archive"),
    ("appimage", "application/x-executable"),
    ("deb", "application/vnd.debian.binary-package"),
    ("exe", "application/x-msdownload"),
    ("iso", "application/x-iso9660-image"),
    ("o", "application/x-object"),
    ("so", "application/x-sharedlib"),
    ("wasm", "application/wasm"),
];

/// Names with no extension that still have a type everyone knows.
const WHOLE_NAMES: &[(&str, &str)] = &[
    ("dockerfile", "text/plain"),
    ("license", "text/plain"),
    ("makefile", "text/x-makefile"),
    ("readme", "text/plain"),
];

/// The mime hint for a file name. `.tar.gz` is read as `gzip`, not `tar`:
/// the last extension is the one that says how to open it.
pub fn hint_for_name(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if let Some((_, mime)) = WHOLE_NAMES.iter().find(|(n, _)| *n == lower) {
        return mime;
    }
    // A leading dot is the hidden marker, not an extension: `.gitignore` is a
    // file called gitignore, not a `gitignore`-typed file. Skipping it here is
    // what keeps `.zshrc` from being reported as a Z-shell script *and* what
    // keeps `.ts` from being a video.
    let stem = lower.strip_prefix('.').unwrap_or(&lower);
    let Some((_, ext)) = stem.rsplit_once('.') else {
        return UNKNOWN_MIME;
    };
    match EXTENSIONS.iter().find(|(e, _)| *e == ext) {
        Some((_, mime)) => disambiguate(ext, stem, mime),
        None => UNKNOWN_MIME,
    }
}

/// The one extension that means two different things in this house.
///
/// `.ts` is MPEG transport stream *and* TypeScript, and Brian's machine has far
/// more of the second. Transport streams travel with a sibling `.m2ts`/`.mts`
/// or a numeric name (`00001.ts`); source files do not. So: a `.ts` whose stem
/// is all digits is video, everything else is TypeScript. Wrong occasionally,
/// wrong in the direction that opens an editor rather than a video player.
fn disambiguate(ext: &str, stem: &str, mime: &'static str) -> &'static str {
    if ext == "ts" {
        let base = stem.rsplit_once('.').map(|(b, _)| b).unwrap_or("");
        if !base.is_empty() && base.chars().all(|c| c.is_ascii_digit()) {
            return mime;
        }
        return "text/typescript";
    }
    mime
}

/// Whether a mime type is one the text previewer can render.
pub fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/ndjson"
                | "application/toml"
                | "application/xml"
                | "application/x-yaml"
                | "application/x-shellscript"
        )
}
