//! What *sort of thing* a row is — the classification the icon column draws
//! from (PLAN §3, PLAN §8).
//!
//! A file manager that draws one glyph for "file" is a list of identical rows
//! with different words on them. A Downloads folder is the case that proves it:
//! twenty jpgs, four pdfs, a zip, an iso and a stray `.txt` all look the same,
//! and the eye has to *read* every name to find the one it wants. So the list
//! needs a word for what a file is, and this is that word.
//!
//! ## It is deliberately not a second type table
//!
//! df-core already knows three things about a name:
//!
//! - [`crate::fs::mime::hint_for_name`] — the mime hint, from the extension.
//! - [`crate::preview::kind::kind_for_mime`] — which previewer a type gets.
//! - [`crate::preview::syntax::syntax_for_name`] — which language a source file
//!   is written in, from a table that already knows `zig`, `nix`, `kt` and
//!   ninety others.
//!
//! [`FileKind`] is built on all three rather than beside them. That is also
//! what makes the *code* bucket free: `syntax_for_name` returning a language is
//! exactly the question "is this source", already answered, already tested, and
//! already covering languages the mime table has never heard of.
//!
//! What is left is a small override table — [`EXTENSIONS`] — for the handful of
//! distinctions none of those three can make: an office document from a socket
//! (both are `Unsupported` to the previewer), an `.iso` from a `.zip` (both are
//! `Archive`), a `.toml` from a `.rs` (both are source to the highlighter), and
//! an `.AppImage` from any other blob.
//!
//! ## Order, which is the specification
//!
//! 1. **What it is on the filesystem** outranks what it is called — a fifo
//!    named `photo.png` is a fifo.
//! 2. **The extension override**, for the distinctions above.
//! 3. **The type, when it says media** — which is how `00001.ts` stays a
//!    transport stream while `app.ts` stays TypeScript.
//! 4. **The language table**, which answers for every source file.
//! 5. **The previewer's own table**, for everything left.
//! 6. **Low salience** — a dotfile or a lockfile that nothing above claimed is
//!    configuration, not a mystery.
//! 7. **The executable bit**, which turns a nameless blob into something you
//!    can run.
//!
//! ## Why there is no `Symlink` kind
//!
//! A link to a photo *is* a photo, and the icon column's job is to say what a
//! row is. That a row is reached through a link is a different fact, already
//! said twice — by the `→ target` suffix after the name and by the colour the
//! painter gives a link — so spending the one glyph the column has on it would
//! trade the useful fact for the decorative one. A **broken** link is its own
//! kind, because then there is no other fact to say.

use crate::fs::mime::{BROKEN_LINK_MIME, DIR_MIME};
use crate::fs::{Entry, Kind, LinkTarget};
use crate::preview::kind::{kind_for_mime, PreviewKind};
use crate::preview::syntax::syntax_for_name;

/// What a row *is*, at the granularity a person scans a column at.
///
/// Sixteen buckets rather than a hundred: the list is read at a glance, and a
/// vocabulary bigger than the eye can hold is a vocabulary nobody learns. Each
/// one is a thing you would *do* differently — look at it, play it, unpack it,
/// read it, run it — which is the only reason to tell two files apart in a file
/// manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Directory,
    /// A symlink to something that exists but is neither a file nor a
    /// directory, plus sockets, fifos and device nodes: real, listed, and not
    /// openable.
    Special,
    BrokenLink,
    Image,
    Video,
    Audio,
    Archive,
    /// `iso`, `img`, `dmg`, `qcow2` — an archive you mount rather than unpack.
    DiskImage,
    /// PDFs, office documents and e-books: things that are read as pages.
    Document,
    Markdown,
    /// Prose. A `.txt`, a `LICENSE`, a log.
    Text,
    /// Source, in any of the languages
    /// [`crate::preview::syntax::syntax_for_name`] knows.
    Code,
    /// Structured data a program reads: json, yaml, toml, csv, xml, sql.
    Data,
    /// Dotfiles, `.conf`/`.ini`, lockfiles — the low-salience rows. Present in
    /// every directory, looked at almost never.
    Config,
    /// Something you can run: an `.AppImage`, an ELF with the bit set, a shell
    /// script, a shared library.
    Executable,
    Font,
    /// stl/obj/gltf/gcode — the turntable and the toolpath.
    Model3d,
    /// Bytes with no better word. The fallback, not a claim.
    Binary,
}

impl FileKind {
    /// Whether this kind is one the list should draw *quietly* — present,
    /// legible, and not competing with the rows you came here for.
    ///
    /// Configuration only. A hidden `.jpg` is still a picture and is still
    /// worth seeing; what earns the quiet treatment is a file whose whole role
    /// is to be there rather than to be read.
    pub fn is_low_salience(self) -> bool {
        matches!(self, FileKind::Config)
    }

    /// Whether a row should carry the "you can run this" mark.
    pub fn is_runnable(self) -> bool {
        matches!(self, FileKind::Executable)
    }

    /// A short lowercase word, for the spot card and for tests. Stable — it is
    /// user-visible text, not a debug spelling.
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Directory => "directory",
            FileKind::Special => "special",
            FileKind::BrokenLink => "broken link",
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Archive => "archive",
            FileKind::DiskImage => "disk image",
            FileKind::Document => "document",
            FileKind::Markdown => "markdown",
            FileKind::Text => "text",
            FileKind::Code => "code",
            FileKind::Data => "data",
            FileKind::Config => "config",
            FileKind::Executable => "executable",
            FileKind::Font => "font",
            FileKind::Model3d => "3d model",
            FileKind::Binary => "binary",
        }
    }
}

/// The override table: extension → kind, for the distinctions no other table in
/// df-core can make. Lowercase, sorted by family, and checked **first** — the
/// task is "read a Downloads folder at a glance", and the extension is what a
/// person is reading.
const EXTENSIONS: &[(&str, FileKind)] = &[
    // Documents. To the previewer these are `Unsupported`, which is also what a
    // socket is; here they are the thing you open to read.
    ("pdf", FileKind::Document),
    ("doc", FileKind::Document),
    ("docx", FileKind::Document),
    ("odt", FileKind::Document),
    ("ods", FileKind::Document),
    ("odp", FileKind::Document),
    ("xls", FileKind::Document),
    ("xlsx", FileKind::Document),
    ("ppt", FileKind::Document),
    ("pptx", FileKind::Document),
    ("rtf", FileKind::Document),
    ("epub", FileKind::Document),
    ("mobi", FileKind::Document),
    ("azw3", FileKind::Document),
    ("djvu", FileKind::Document),
    // Structured data. The highlighter calls these source, which is true and
    // not what the column is asking.
    ("json", FileKind::Data),
    ("jsonc", FileKind::Data),
    ("jsonl", FileKind::Data),
    ("ndjson", FileKind::Data),
    ("yaml", FileKind::Data),
    ("yml", FileKind::Data),
    ("toml", FileKind::Data),
    ("csv", FileKind::Data),
    ("tsv", FileKind::Data),
    ("xml", FileKind::Data),
    ("plist", FileKind::Data),
    ("sql", FileKind::Data),
    ("parquet", FileKind::Data),
    ("db", FileKind::Data),
    ("sqlite", FileKind::Data),
    ("sqlite3", FileKind::Data),
    // Configuration — see [`FileKind::Config`].
    ("conf", FileKind::Config),
    ("cfg", FileKind::Config),
    ("ini", FileKind::Config),
    ("lock", FileKind::Config),
    ("desktop", FileKind::Config),
    ("service", FileKind::Config),
    ("properties", FileKind::Config),
    // Something you can run.
    ("appimage", FileKind::Executable),
    ("exe", FileKind::Executable),
    ("msi", FileKind::Executable),
    ("so", FileKind::Executable),
    ("dylib", FileKind::Executable),
    ("dll", FileKind::Executable),
    ("o", FileKind::Executable),
    ("a", FileKind::Executable),
    ("wasm", FileKind::Executable),
    ("sh", FileKind::Executable),
    ("bash", FileKind::Executable),
    ("zsh", FileKind::Executable),
    ("fish", FileKind::Executable),
    ("ps1", FileKind::Executable),
    // A disc, not a bundle: `.iso` is `Archive` to the previewer because
    // listing its contents is a real answer, and a *disk image* to the eye.
    ("iso", FileKind::DiskImage),
    ("img", FileKind::DiskImage),
    ("dmg", FileKind::DiskImage),
    ("qcow2", FileKind::DiskImage),
    ("vdi", FileKind::DiskImage),
    ("vmdk", FileKind::DiskImage),
    ("vhd", FileKind::DiskImage),
    // Archives the mime table has no entry for, because nothing previews them.
    ("tgz", FileKind::Archive),
    ("txz", FileKind::Archive),
    ("tbz", FileKind::Archive),
    ("tbz2", FileKind::Archive),
    ("lz4", FileKind::Archive),
    ("lzma", FileKind::Archive),
    ("cab", FileKind::Archive),
    ("jar", FileKind::Archive),
    ("whl", FileKind::Archive),
    ("gem", FileKind::Archive),
    ("rpm", FileKind::Archive),
    ("pkg", FileKind::Archive),
    ("apk", FileKind::Archive),
    ("xpi", FileKind::Archive),
    // Pictures the mime table stops short of: camera raw and the editable
    // formats, which are pictures to a person and octet-streams to a sniffer.
    ("svg", FileKind::Image),
    ("dng", FileKind::Image),
    ("cr2", FileKind::Image),
    ("cr3", FileKind::Image),
    ("nef", FileKind::Image),
    ("arw", FileKind::Image),
    ("raf", FileKind::Image),
    ("orf", FileKind::Image),
    ("psd", FileKind::Image),
    ("xcf", FileKind::Image),
    ("kra", FileKind::Image),
    ("ai", FileKind::Image),
    // …and the containers it stops short of.
    ("flv", FileKind::Video),
    ("3gp", FileKind::Video),
    ("mts", FileKind::Video),
    ("m2ts", FileKind::Video),
    ("vob", FileKind::Video),
    ("ogv", FileKind::Video),
    ("mid", FileKind::Audio),
    ("midi", FileKind::Audio),
    ("ape", FileKind::Audio),
    ("wv", FileKind::Audio),
    ("dsf", FileKind::Audio),
    // Geometry and toolpaths beyond the four the previewer turntables.
    ("gltf", FileKind::Model3d),
    ("glb", FileKind::Model3d),
    ("fbx", FileKind::Model3d),
    ("dae", FileKind::Model3d),
    ("blend", FileKind::Model3d),
    ("step", FileKind::Model3d),
    ("stp", FileKind::Model3d),
    ("bgcode", FileKind::Model3d),
    // Prose that nothing else claims.
    ("txt", FileKind::Text),
    ("log", FileKind::Text),
    ("nfo", FileKind::Text),
];

/// Whole names that are configuration whatever their extension says.
///
/// `package-lock.json` is not data you would ever read; it is the lockfile, and
/// a directory that shows it as loudly as `package.json` has told you nothing.
const CONFIG_NAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "cargo.lock",
    "flake.lock",
    "poetry.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".dockerignore",
    ".editorconfig",
    ".npmrc",
    ".nvmrc",
    ".prettierrc",
    ".eslintrc",
];

/// The kind of one listed row.
///
/// A field read, not a computation: [`classify`] answered this once when the
/// entry was built, because the icon column and the name colour both ask it for
/// every visible row on every frame and the answer cannot change without the
/// row being scanned again.
pub fn kind_of(entry: &Entry) -> FileKind {
    entry.file_kind
}

/// Classify from the four facts a scan has in hand, before there is an
/// [`Entry`] to hang the answer on.
///
/// Takes all four because three of the six steps need something that is not the
/// name: the filesystem kind, the mime the scanner attached, and the permission
/// bits.
pub fn classify(kind: Kind, name: &str, mime: &str, mode: u32) -> FileKind {
    let is_dir = matches!(
        kind,
        Kind::Dir
            | Kind::Symlink {
                target: Some(LinkTarget::Dir)
            }
    );
    if is_dir || mime == DIR_MIME {
        return FileKind::Directory;
    }
    match kind {
        Kind::Symlink { target: None } => return FileKind::BrokenLink,
        Kind::Symlink {
            target: Some(LinkTarget::Other),
        } => return FileKind::Special,
        _ => {}
    }
    if mime == BROKEN_LINK_MIME {
        return FileKind::BrokenLink;
    }
    // A socket, fifo or device node reached directly rather than through a
    // link. `Kind::File` is everything that is not a directory and not a
    // symlink, so the file-type bits of the mode are the only place the
    // distinction survives — and a fifo named `photo.png` is a fifo (step 1).
    if is_special_mode(mode) {
        return FileKind::Special;
    }
    kind_for_name(name, mime, mode)
}

/// The `S_IFMT` bits of an `st_mode`, when they are there to read.
///
/// Rows that never came from a `stat` — an archive's tree, a remote listing
/// that answered with permissions only — carry a mode with no type bits at
/// all, and "0" means *not known*, not "not a regular file".
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;

/// Is this mode a stat'd non-regular file? False for an unknown mode.
fn is_special_mode(mode: u32) -> bool {
    let file_type = mode & S_IFMT;
    file_type != 0 && file_type != S_IFREG
}

/// The name-and-type half of [`kind_of`], so the table can be tested without
/// building an [`Entry`] per row. See the module header for why the order of
/// the checks is the specification.
pub fn kind_for_name(name: &str, mime: &str, mode: u32) -> FileKind {
    let kind = named_kind(name)
        .or_else(|| extension_kind(name))
        .or_else(|| media_kind(mime, name))
        .or_else(|| language_kind(name))
        .unwrap_or_else(|| from_preview(kind_for_mime(mime, name)));

    // A dotfile nothing above recognised is configuration, not a mystery: a
    // `.zshrc` and a `.gitconfig` are exactly the rows the eye should skip.
    // Only the buckets that mean "we could not say" are demoted — a hidden
    // `.wallpaper.png` is still a picture.
    //
    // **`Code` is only demoted when the name has no extension**, which is the
    // shape the rule was written for: `.zshrc` and `.gitconfig` are recognised
    // as shell and INI by the highlighter's table and are still configuration
    // to a person reading a listing. `.eslintrc.js` is not that shape. It has
    // an extension, that extension is the whole reason anything knows what the
    // file is, and demoting it threw the answer away — after which df-app's
    // icon table, which vetoes a glyph whose kind disagrees with the
    // classifier, refused to draw the JavaScript logo and put a cog on it. Two
    // rules that are each right on their own, meeting on a file that says what
    // it is in its own name.
    let unnamed = match kind {
        FileKind::Binary | FileKind::Text => true,
        // Its extension named a language: keep it.
        FileKind::Code => extension_of(name).is_none(),
        _ => false,
    };
    let kind = if name.starts_with('.') && unnamed {
        FileKind::Config
    } else {
        kind
    };

    // Last, because it is the weakest signal: the bit says a blob can be run,
    // and says nothing at all about a `.png` that someone chmod'd. It says
    // nothing about a socket either — every socket on the machine is 0755, and
    // reading that as "you can run this" was the bug — so the rule only applies
    // to a file the mode says is regular, or to a mode with no type bits to ask.
    // Whether it runs is the platform's to say: an execute bit on Unix, the
    // extension (`PATHEXT`) on Windows, which has no bit.
    if kind == FileKind::Binary
        && crate::platform::meta::is_executable(std::ffi::OsStr::new(name), mode)
        && !is_special_mode(mode)
    {
        return FileKind::Executable;
    }
    kind
}

/// The whole names, matched without regard to case and **without lowercasing
/// the name to do it**: this runs once per entry in a directory that may hold
/// two hundred thousand of them, and a `String` per row to answer a table
/// lookup is a table lookup that allocates.
fn named_kind(name: &str) -> Option<FileKind> {
    CONFIG_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
        .then_some(FileKind::Config)
        .or_else(|| {
            // `README`, `LICENSE`, `CHANGELOG` with no extension at all.
            PROSE_NAMES
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name))
                .then_some(FileKind::Text)
        })
}

/// The names that are prose whatever their case: `README`, `readme`, `ReadMe`.
const PROSE_NAMES: &[&str] = &["readme", "license", "licence", "copying", "authors"];

fn extension_kind(name: &str) -> Option<FileKind> {
    let ext = extension_of(name)?;
    EXTENSIONS
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, kind)| *kind)
}

/// The part after the last dot, or `None` for a name that has no extension.
///
/// A leading dot is the hidden marker, not an extension — the rule
/// [`crate::fs::mime::hint_for_name`] follows, for the same reason. Returns a
/// slice of `name` rather than an owned lowercase copy; the callers compare
/// case-insensitively instead.
pub fn extension_of(name: &str) -> Option<&str> {
    let stem = name.strip_prefix('.').unwrap_or(name);
    stem.rsplit_once('.').map(|(_, ext)| ext)
}

/// Media, when the *type* says so — checked ahead of the language table
/// because [`crate::fs::mime`] has already done the one piece of thinking a
/// name cannot: `00001.ts` is a transport stream and `app.ts` is TypeScript,
/// and the highlighter's table calls both of them source.
fn media_kind(mime: &str, name: &str) -> Option<FileKind> {
    let kind = from_preview(kind_for_mime(mime, name));
    matches!(
        kind,
        FileKind::Image | FileKind::Video | FileKind::Audio | FileKind::Font | FileKind::Model3d
    )
    .then_some(kind)
}

/// Source, via the table the syntax highlighter already keeps. The three
/// language names that are *not* code in a file listing are split back out —
/// markdown is read, and JSON/INI are configuration or data — but everything
/// else the highlighter has a grammar for is source.
fn language_kind(name: &str) -> Option<FileKind> {
    Some(match syntax_for_name(name)? {
        "Markdown" => FileKind::Markdown,
        "JSON" | "YAML" | "TOML" | "XML" | "CSV" | "SQL" => FileKind::Data,
        "INI" => FileKind::Config,
        _ => FileKind::Code,
    })
}

/// The previewer's answer, translated. Reached only for the types no extension
/// and no language claimed — which is to say, for media.
fn from_preview(kind: PreviewKind) -> FileKind {
    match kind {
        PreviewKind::Directory => FileKind::Directory,
        PreviewKind::Image => FileKind::Image,
        PreviewKind::Video => FileKind::Video,
        PreviewKind::Audio => FileKind::Audio,
        PreviewKind::Pdf => FileKind::Document,
        PreviewKind::Font => FileKind::Font,
        PreviewKind::Model3d | PreviewKind::Gcode => FileKind::Model3d,
        PreviewKind::Archive => FileKind::Archive,
        PreviewKind::Markdown => FileKind::Markdown,
        // `Unsupported` reaching here is an office family prefix; the sockets
        // and broken links that share the variant were answered by `kind_of`
        // before the mime was ever consulted.
        PreviewKind::Unsupported => FileKind::Document,
        PreviewKind::Text { syntax: Some(_) } => FileKind::Code,
        PreviewKind::Text { syntax: None } => FileKind::Text,
        // Zero bytes, unreadable, or genuinely unrecognised: three different
        // facts, none of them a *kind*, and the generic bucket is the honest
        // answer to all three.
        PreviewKind::Empty | PreviewKind::Denied | PreviewKind::Binary => FileKind::Binary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::fs::mime::hint_for_name;

    /// The classifier as a row would reach it: the hint the scanner attaches,
    /// and ordinary permissions.
    fn kind(name: &str) -> FileKind {
        kind_for_name(name, hint_for_name(name), 0o644)
    }

    fn entry(name: &str, k: Kind, mime: &'static str) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            kind: k,
            len: 10,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            is_hidden: name.starts_with('.'),
            mime,
            file_kind: classify(k, name, mime, 0o644),
            tags: Vec::new(),
        }
    }

    /// The Downloads folder from the brief, one row at a time. This is the
    /// test the feature exists for.
    #[test]
    fn a_downloads_folder_reads_as_itself() {
        let cases: &[(&str, FileKind)] = &[
            ("holiday.jpg", FileKind::Image),
            ("screenshot.png", FileKind::Image),
            ("IMG_4821.HEIC", FileKind::Image),
            ("logo.svg", FileKind::Image),
            ("loop.gif", FileKind::Image),
            ("raw.CR3", FileKind::Image),
            ("invoice.pdf", FileKind::Document),
            ("contract.docx", FileKind::Document),
            ("book.epub", FileKind::Document),
            ("release.zip", FileKind::Archive),
            ("src.tar.gz", FileKind::Archive),
            ("bundle.7z", FileKind::Archive),
            ("photos.tgz", FileKind::Archive),
            ("clip.mov", FileKind::Video),
            ("film.mkv", FileKind::Video),
            ("trailer.mp4", FileKind::Video),
            ("song.mp3", FileKind::Audio),
            ("album.flac", FileKind::Audio),
            ("arch.iso", FileKind::DiskImage),
            ("notes.txt", FileKind::Text),
            ("README.md", FileKind::Markdown),
        ];
        for (name, expected) in cases {
            assert_eq!(kind(name), *expected, "for {name}");
        }
    }

    /// Source is answered by the highlighter's table, which is why languages
    /// with no mime entry at all still land in the right bucket.
    #[test]
    fn source_files_are_code_even_without_a_mime() {
        for name in [
            "main.rs",
            "app.ts",
            "index.tsx",
            "server.js",
            "train.py",
            "main.go",
            "lib.c",
            "widget.svelte",
            "build.zig",
            "flake.nix",
            "Main.kt",
        ] {
            assert_eq!(kind(name), FileKind::Code, "for {name}");
        }
    }

    /// …and the three language names that are not source in a listing.
    #[test]
    fn data_and_prose_are_not_source() {
        assert_eq!(kind("package.json"), FileKind::Data);
        assert_eq!(kind("compose.yaml"), FileKind::Data);
        assert_eq!(kind("Cargo.toml"), FileKind::Data);
        assert_eq!(kind("rows.csv"), FileKind::Data);
        assert_eq!(kind("schema.sql"), FileKind::Data);
        assert_eq!(kind("notes.md"), FileKind::Markdown);
        assert_eq!(kind("LICENSE"), FileKind::Text);
    }

    /// The low-salience rows: dotfiles, `.conf`, and every lockfile — including
    /// the ones whose extension claims they are data.
    #[test]
    fn configuration_is_low_salience() {
        for name in [
            ".zshrc",
            ".gitignore",
            ".editorconfig",
            "nginx.conf",
            "settings.ini",
            "Cargo.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "flake.lock",
        ] {
            assert_eq!(kind(name), FileKind::Config, "for {name}");
            assert!(kind(name).is_low_salience(), "for {name}");
        }
        // A hidden picture is still a picture: the demotion only claims the
        // rows nothing else recognised.
        assert_eq!(kind(".wallpaper.png"), FileKind::Image);
        assert!(!FileKind::Image.is_low_salience());
    }

    #[test]
    fn things_you_can_run_are_executable() {
        assert_eq!(kind("Zed.AppImage"), FileKind::Executable);
        assert_eq!(kind("deploy.sh"), FileKind::Executable);
        assert_eq!(kind("libfoo.so"), FileKind::Executable);
        assert_eq!(kind("game.exe"), FileKind::Executable);
        assert_eq!(
            classify(
                Kind::File,
                "setup.exe",
                "application/x-msdownload",
                0o100_644
            ),
            FileKind::Executable,
            "by its name, on every platform"
        );
        // A blob the name cannot answer for: the platform says whether it
        // runs — the execute bit on Unix, the extension (`PATHEXT`) on
        // Windows, which has no bit.
        assert_eq!(
            kind_for_name("a.out", "application/octet-stream", 0o755) == FileKind::Executable,
            cfg!(unix)
        );
        assert_eq!(
            kind_for_name("a.out", "application/octet-stream", 0o644),
            FileKind::Binary
        );
        assert_eq!(
            kind_for_name("OLD.COM", "application/octet-stream", 0o644) == FileKind::Executable,
            cfg!(windows)
        );
        // …and the bit on a picture means nothing.
        assert_eq!(
            kind_for_name("photo.png", "image/png", 0o755),
            FileKind::Image
        );
        assert!(FileKind::Executable.is_runnable());
        assert!(!FileKind::Code.is_runnable());
    }

    /// What it is on the filesystem outranks what it is called — the same rule
    /// the previewer's table follows, for the same reason.
    #[test]
    fn what_it_is_outranks_what_it_is_called() {
        let dir = entry("src", Kind::Dir, DIR_MIME);
        assert_eq!(kind_of(&dir), FileKind::Directory);

        let broken = entry("gone.png", Kind::Symlink { target: None }, "image/png");
        assert_eq!(kind_of(&broken), FileKind::BrokenLink);

        let fifo = entry(
            "photo.png",
            Kind::Symlink {
                target: Some(LinkTarget::Other),
            },
            "image/png",
        );
        assert_eq!(kind_of(&fifo), FileKind::Special);

        // A symlink to a real file is classified as what it points at: the
        // arrow already says it is a link, and colouring it "link" would lose
        // the one fact the icon column is for.
        let link = entry(
            "shot.png",
            Kind::Symlink {
                target: Some(LinkTarget::File),
            },
            "image/png",
        );
        assert_eq!(kind_of(&link), FileKind::Image);
    }

    /// Sockets, fifos and device nodes reached *directly* — no symlink in the
    /// way, so `Kind::File` is all the scanner can say and the mode's type bits
    /// are the whole signal.
    #[test]
    fn a_socket_is_special_and_not_executable() {
        // The kind is fixed when the entry is built, so the mode is set the
        // way a scan would set it: through `classify`, not by editing the row
        // afterwards.
        let with_mode = |name: &str, mime: &'static str, mode: u32| {
            let mut e = entry(name, Kind::File, mime);
            e.mode = mode;
            e.file_kind = classify(Kind::File, name, mime, mode);
            e
        };
        // Every socket on the machine: 0755, and the exec bit used to win.
        let sock = with_mode("S.gpg-agent", "application/octet-stream", 0o140_000 | 0o755);
        assert_eq!(kind_of(&sock), FileKind::Special);

        let fifo = with_mode("photo.png", "image/png", 0o010_000 | 0o644);
        assert_eq!(kind_of(&fifo), FileKind::Special);

        let device = with_mode("null", "application/octet-stream", 0o020_000 | 0o666);
        assert_eq!(kind_of(&device), FileKind::Special);

        let block = with_mode("sda", "application/octet-stream", 0o060_000 | 0o660);
        assert_eq!(kind_of(&block), FileKind::Special);

        // A regular file with the same bits is still what it was: runnable
        // where the bit says so (Unix; Windows asks the extension instead).
        let script = with_mode("run", "application/octet-stream", 0o100_000 | 0o755);
        assert_eq!(
            kind_of(&script) == FileKind::Executable,
            cfg!(unix),
            "{:?}",
            kind_of(&script)
        );

        let photo = with_mode("holiday.jpg", "image/jpeg", 0o100_000 | 0o644);
        assert_eq!(kind_of(&photo), FileKind::Image);
    }

    /// A row that never came from a `stat` — an archive's tree, a remote
    /// listing — has no type bits, and "0" must not read as "not a file".
    #[test]
    fn a_mode_with_no_type_bits_classifies_as_before() {
        assert_eq!(
            kind_for_name("holiday.jpg", "image/jpeg", 0o644),
            FileKind::Image
        );
        assert_eq!(
            kind_for_name("run", "application/octet-stream", 0o755) == FileKind::Executable,
            cfg!(unix),
            "the bit, where the platform reads one"
        );
        let entry = entry("run", Kind::File, "application/octet-stream");
        assert_eq!(entry.mode & 0o170_000, 0);
        assert_eq!(kind_of(&entry), FileKind::Binary);
    }

    /// The `.ts` ambiguity survives: the mime hint disambiguates it and this
    /// table deliberately does not override the answer.
    #[test]
    fn typescript_and_transport_streams_stay_apart() {
        assert_eq!(kind("app.ts"), FileKind::Code);
        assert_eq!(kind("00001.ts"), FileKind::Video);
    }

    #[test]
    fn every_label_is_distinct_and_lowercase() {
        let kinds = [
            FileKind::Directory,
            FileKind::Special,
            FileKind::BrokenLink,
            FileKind::Image,
            FileKind::Video,
            FileKind::Audio,
            FileKind::Archive,
            FileKind::DiskImage,
            FileKind::Document,
            FileKind::Markdown,
            FileKind::Text,
            FileKind::Code,
            FileKind::Data,
            FileKind::Config,
            FileKind::Executable,
            FileKind::Font,
            FileKind::Model3d,
            FileKind::Binary,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in kinds {
            assert!(seen.insert(k.label()), "{:?} repeats a label", k);
            assert_eq!(k.label(), k.label().to_lowercase());
        }
    }

    /// A table with the same extension twice is a rule that can never fire.
    #[test]
    fn the_override_table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (ext, _) in EXTENSIONS {
            assert!(seen.insert(*ext), "{ext} appears twice");
            assert_eq!(*ext, ext.to_lowercase(), "{ext} is not lowercase");
        }
        for name in CONFIG_NAMES {
            assert_eq!(*name, name.to_lowercase(), "{name} is not lowercase");
        }
    }
}
