//! The questions about a local path that `std::path` does not answer, asked
//! the same way on every platform.
//!
//! df-core was written against Unix paths: one root, `/`; names that are
//! bytes; one spelling per file. Windows has a root per drive and per share
//! (`C:\`, `\\server\share\`), two separators, names that are UTF-16, and one
//! file behind every spelling that differs only in case. `std::path` already
//! parses all of that correctly, so the rule for the rest of the crate is to
//! let it — `components`, `file_name`, `parent`, `join` — and never to split
//! on a literal `/` (`plans/other-platforms/00-ground-rules.md` §4). What is
//! left over lives here: what the root of a path is and whether a path is
//! one, how a trailing separator is read and trimmed, how the breadcrumb
//! segments a path, how a lookup table keys one, which names the platform
//! refuses, and how a path is shown.
//!
//! There is no `cfg` in this file. Where the answer differs by platform, the
//! difference is `std::path`'s own parsing (a `C:` is a prefix only on
//! Windows) or one of [`crate::platform::os`]'s constants, and the Windows
//! rule behind each constant is a public function of its own —
//! [`folded`], [`name_is_valid_strict`] — so it is compiled, and tested, on
//! Linux as well.
//!
//! None of this applies to a remote row's path (`sftp://host/…`, carried in a
//! `PathBuf`): that is a URL, and [`crate::vfs::VfsPath`] is what splits it.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{is_separator, Component, Path, PathBuf, Prefix, MAIN_SEPARATOR_STR};

use crate::platform;

/// One step of the breadcrumb: what it says, and where clicking it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub label: String,
    pub path: PathBuf,
}

/// The root `p` hangs from: `/` on Unix; the drive or share with its root
/// separator on Windows (`C:\`, `\\server\share\`). A path with no root of its
/// own — a relative one — hangs from the current directory's; a Windows
/// drive-relative one (`C:notes`) from that drive's root.
pub fn root_of(p: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for component in p.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => {
                root.push(component.as_os_str());
                return root;
            }
            _ => break,
        }
    }
    if root.as_os_str().is_empty() {
        return match std::env::current_dir() {
            Ok(cwd) if cwd.has_root() => root_of(&cwd),
            _ => PathBuf::from(MAIN_SEPARATOR_STR),
        };
    }
    root.push(MAIN_SEPARATOR_STR);
    root
}

/// Whether `p` is a root and nothing more: `/`, `C:\`, `\\server\share`.
/// A bare `C:` is not — it names the current directory of that drive.
pub fn is_root(p: &Path) -> bool {
    p.has_root()
        && p.components()
            .all(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
}

/// Whether `s` ends in a separator: `/` on Unix, `/` or `\` on Windows —
/// what `a notes/` means when it makes a folder rather than a file.
pub fn has_trailing_separator(s: &OsStr) -> bool {
    match platform::os::as_bytes(s) {
        Ok(bytes) => bytes.last().is_some_and(|b| is_separator_byte(*b)),
        Err(_) => s.to_string_lossy().ends_with(is_separator),
    }
}

/// `s` with its trailing separators removed, never cutting into the root:
/// `/` stays `/`, `C:\` stays `C:\` (trimmed, it would be `C:`, the drive's
/// current directory) and `\\server\share\` stays whole. A name the platform
/// cannot spell as bytes (not Unicode, on Windows) is left as it is.
pub fn trim_trailing_separator(s: &OsStr) -> Cow<'_, OsStr> {
    let Ok(bytes) = platform::os::as_bytes(s) else {
        return Cow::Borrowed(s);
    };
    let keep = root_len(Path::new(s)).max(1);
    let mut end = bytes.len();
    while end > keep && is_separator_byte(bytes[end - 1]) {
        end -= 1;
    }
    if end == bytes.len() {
        return Cow::Borrowed(s);
    }
    match platform::os::from_bytes(&bytes[..end]) {
        Ok(trimmed) => Cow::Owned(trimmed),
        Err(_) => Cow::Borrowed(s),
    }
}

/// How many bytes at the front of `p` are its root — prefix and root
/// separator — in the byte form [`platform::os::as_bytes`] gives.
fn root_len(p: &Path) -> usize {
    let mut len = 0;
    for component in p.components() {
        match component {
            Component::Prefix(prefix) => len += prefix.as_os_str().len(),
            // One separator, however many were typed: the rest are trailing
            // ones a trim may take.
            Component::RootDir => return len + 1,
            _ => break,
        }
    }
    len
}

fn is_separator_byte(b: u8) -> bool {
    b.is_ascii() && is_separator(char::from(b))
}

/// The breadcrumb's steps for `p`, root first. On Unix the first is `/`; on
/// Windows it is the drive or share as people write it (`C:`,
/// `\\server\share`), and it goes to that root. Then one per name, each going
/// to the path up to and including it.
pub fn segments(p: &Path) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut at = PathBuf::new();
    let mut prefix: Option<String> = None;
    for component in p.components() {
        match component {
            Component::Prefix(drive) => {
                at.push(drive.as_os_str());
                prefix = Some(prefix_label(drive.kind()));
            }
            Component::RootDir => {
                at.push(component.as_os_str());
                let label = prefix
                    .take()
                    .unwrap_or_else(|| component.as_os_str().to_string_lossy().into_owned());
                out.push(Segment {
                    label,
                    path: at.clone(),
                });
            }
            Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                // A drive with no root after it (`C:notes`) is a step of its
                // own, going to that drive's current directory.
                if let Some(label) = prefix.take() {
                    out.push(Segment {
                        label,
                        path: at.clone(),
                    });
                }
                at.push(component.as_os_str());
                out.push(Segment {
                    label: component.as_os_str().to_string_lossy().into_owned(),
                    path: at.clone(),
                });
            }
        }
    }
    if let Some(label) = prefix {
        out.push(Segment { label, path: at });
    }
    out
}

/// A Windows prefix as a person writes it: `C:` for `C:` and `\\?\C:`,
/// `\\server\share` for a share however it was spelled.
fn prefix_label(prefix: Prefix<'_>) -> String {
    match prefix {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => format!("{}:", char::from(drive)),
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
            r"\\{}\{}",
            server.to_string_lossy(),
            share.to_string_lossy()
        ),
        Prefix::Verbatim(name) => format!(r"\\?\{}", name.to_string_lossy()),
        Prefix::DeviceNS(name) => format!(r"\\.\{}", name.to_string_lossy()),
    }
}

/// The key a table indexed by path stores and looks `p` up under, so that
/// two spellings of one file find one record. `p` itself where spellings are
/// files (Unix, macOS: see [`platform::os::FOLD_CASE`]); [`folded`] on
/// Windows. Key at insert **and** at lookup; keep the spelling beside the key
/// when it is shown to anyone.
pub fn key(p: &Path) -> Cow<'_, Path> {
    if platform::os::FOLD_CASE {
        Cow::Owned(folded(p))
    } else {
        Cow::Borrowed(p)
    }
}

/// [`key`] for a path the caller owns and is about to store: no copy where
/// the key is the path.
pub fn into_key(p: PathBuf) -> PathBuf {
    if platform::os::FOLD_CASE {
        folded(&p)
    } else {
        p
    }
}

/// [`into_key`], and the spelling beside it when the key differs from it —
/// for a table that shows its keys to anyone, as names or paths. `(p, None)`
/// where the key is the path, so Unix keeps one copy.
pub fn keyed(p: PathBuf) -> (PathBuf, Option<PathBuf>) {
    if !platform::os::FOLD_CASE {
        return (p, None);
    }
    let key = folded(&p);
    if key == p {
        (key, None)
    } else {
        (key, Some(p))
    }
}

/// `p` with every name lowercased — the fold [`key`] applies on Windows, by
/// Unicode's lowercase mapping of its UTF-8 view. A path that is not Unicode
/// is left as it is.
pub fn folded(p: &Path) -> PathBuf {
    match p.to_str() {
        Some(text) => PathBuf::from(text.to_lowercase()),
        None => p.to_path_buf(),
    }
}

/// Whether `name` may be made on this platform, and if not, why — in words
/// for a toast or a bulk-rename row. On Unix only NUL and `/` are refused,
/// which is all the file system refuses; on Windows,
/// [`name_is_valid_strict`]'s rules.
pub fn name_is_valid(name: &OsStr) -> Result<(), &'static str> {
    if platform::os::STRICT_NAMES {
        name_is_valid_strict(name)
    } else {
        name_is_valid_permissive(name)
    }
}

/// The Unix rule: no NUL, no `/`.
pub fn name_is_valid_permissive(name: &OsStr) -> Result<(), &'static str> {
    let text = name.to_string_lossy();
    if text.contains('\0') {
        return Err("a name cannot contain a NUL character");
    }
    if text.contains('/') {
        return Err("a name cannot contain /");
    }
    Ok(())
}

/// Windows' rule: no `/` or `\`; none of `< > : " | ? *`; no control
/// character; no trailing dot or space (Windows drops them, so the file made
/// would not be the name asked for); and not a device name — `CON`, `PRN`,
/// `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, in any case, with or without an
/// extension (`con.txt` is the console too).
pub fn name_is_valid_strict(name: &OsStr) -> Result<(), &'static str> {
    let text = name.to_string_lossy();
    if text.contains(['/', '\\']) {
        return Err("a name cannot contain / or \\");
    }
    if text.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Err("a name cannot contain any of < > : \" | ? *");
    }
    if text.chars().any(|c| u32::from(c) < 0x20) {
        return Err("a name cannot contain a control character");
    }
    if text.ends_with(['.', ' ']) {
        return Err("a name cannot end in a dot or a space");
    }
    let base = text.split('.').next().unwrap_or_default().trim_end();
    if is_device_name(base) {
        return Err("CON, PRN, AUX, NUL, COM1–9 and LPT1–9 are the names of devices");
    }
    Ok(())
}

fn is_device_name(base: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    if DEVICES.iter().any(|d| d.eq_ignore_ascii_case(base)) {
        return true;
    }
    let mut chars = base.chars();
    let head: String = chars.by_ref().take(3).collect();
    let digit = chars.next();
    let is_port = head.eq_ignore_ascii_case("COM") || head.eq_ignore_ascii_case("LPT");
    is_port && chars.next().is_none() && matches!(digit, Some('1'..='9' | '¹' | '²' | '³'))
}

/// `p` for a person to read: [`Path::display`]'s text, with Windows' `\\?\`
/// verbatim prefix taken off (`\\?\C:\x` is `C:\x`, `\\?\UNC\s\sh\x` is
/// `\\s\sh\x`) — `std::fs::canonicalize` answers in that form, and nobody
/// types it.
pub fn display(p: &Path) -> String {
    let mut components = p.components();
    let plain = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => format!("{}:", char::from(drive)),
            Prefix::VerbatimUNC(server, share) => format!(
                r"\\{}\{}",
                server.to_string_lossy(),
                share.to_string_lossy()
            ),
            _ => return p.to_string_lossy().into_owned(),
        },
        _ => return p.to_string_lossy().into_owned(),
    };
    let mut shown = PathBuf::from(plain);
    shown.push(components.as_path());
    shown.to_string_lossy().into_owned()
}

/// A place as a person writes one — `~/Work`, `~\Work`, `/mnt/x`,
/// `sftp://host/srv` — with a leading `~` replaced by the home directory
/// ([`platform::dirs::home`]: `$HOME`, or `%USERPROFILE%` on Windows). The
/// rest is kept as typed, separator and all, so a `~/Work` written on Linux
/// names `C:\Users\x/Work` on Windows, which is the same folder. Unchanged
/// when there is no home, or no `~`.
///
/// The one rule for every `~`: `[goto]` bookmarks, pins, an SFTP key file.
pub fn expand_home(text: &str) -> String {
    let Some(rest) = text.strip_prefix('~') else {
        return text.to_string();
    };
    match platform::dirs::home() {
        Some(home) => format!("{}{}", home.to_string_lossy(), rest),
        None => text.to_string(),
    }
}

/// `text` with this platform's separator written as `/`, for the formats
/// that own the `/` — an archive's member names and link targets, a pattern
/// a config wrote with slashes. The identity on Unix, where a `\` is part of
/// a name; on Windows every `\` becomes a `/`.
pub fn with_slashes(text: &str) -> Cow<'_, str> {
    if std::path::MAIN_SEPARATOR == '/' || !text.contains(std::path::MAIN_SEPARATOR) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.replace(std::path::MAIN_SEPARATOR, "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    fn labels(path: &str) -> Vec<(String, PathBuf)> {
        segments(Path::new(path))
            .into_iter()
            .map(|s| (s.label, s.path))
            .collect()
    }

    #[test]
    fn the_unix_root_is_a_slash() {
        if !cfg!(unix) {
            return;
        }
        assert_eq!(root_of(Path::new("/")), p("/"));
        assert_eq!(root_of(Path::new("/a/b")), p("/"));
        assert_eq!(root_of(Path::new("relative/x")), p("/"));
        assert!(is_root(Path::new("/")));
        assert!(is_root(Path::new("//")));
        assert!(!is_root(Path::new("/a")));
        assert!(!is_root(Path::new("")));
        assert!(!is_root(Path::new("a")));
    }

    #[test]
    fn windows_roots_are_drives_and_shares() {
        if !cfg!(windows) {
            return;
        }
        assert_eq!(root_of(Path::new(r"C:\")), p(r"C:\"));
        assert_eq!(root_of(Path::new(r"C:\a\b")), p(r"C:\"));
        assert_eq!(root_of(Path::new("C:notes")), p(r"C:\"));
        assert_eq!(root_of(Path::new(r"\\s\sh\a")), p(r"\\s\sh\"));
        assert_eq!(root_of(Path::new(r"\\?\C:\a")), p(r"\\?\C:\"));
        assert!(is_root(Path::new(r"C:\")));
        assert!(is_root(Path::new("C:/")));
        assert!(is_root(Path::new(r"\\s\sh")));
        assert!(is_root(Path::new(r"\\s\sh\")));
        assert!(!is_root(Path::new("C:")), "the drive's current directory");
        assert!(!is_root(Path::new(r"C:\a")));
    }

    #[test]
    fn a_trailing_separator_is_read_and_trimmed_but_never_off_the_root() {
        let trimmed = |s: &str| trim_trailing_separator(OsStr::new(s)).into_owned();
        assert!(has_trailing_separator(OsStr::new("notes/")));
        assert!(!has_trailing_separator(OsStr::new("notes")));
        assert!(!has_trailing_separator(OsStr::new("")));
        assert_eq!(trimmed("a/b/"), "a/b");
        assert_eq!(trimmed("a/b//"), "a/b");
        assert_eq!(trimmed("a"), "a");
        assert_eq!(trimmed(""), "");
        if cfg!(unix) {
            assert_eq!(trimmed("/"), "/");
            assert_eq!(trimmed("//"), "/");
            assert_eq!(trimmed("/a/"), "/a");
            assert!(
                !has_trailing_separator(OsStr::new(r"a\")),
                "a backslash is a name's"
            );
            assert_eq!(trimmed(r"a\"), r"a\");
        }
        if cfg!(windows) {
            assert!(has_trailing_separator(OsStr::new(r"notes\")));
            assert_eq!(trimmed(r"C:\"), r"C:\");
            assert_eq!(trimmed(r"C:\\"), r"C:\");
            assert_eq!(trimmed(r"C:\a\"), r"C:\a");
            assert_eq!(trimmed(r"C:\a/\"), r"C:\a");
            assert_eq!(trimmed(r"\\s\sh\"), r"\\s\sh\");
            assert_eq!(trimmed(r"\\s\sh\a\"), r"\\s\sh\a");
        }
    }

    #[test]
    fn segments_go_root_first_then_one_per_name() {
        if cfg!(unix) {
            assert_eq!(labels("/"), [("/".to_string(), p("/"))]);
            assert_eq!(
                labels("/a/b"),
                [
                    ("/".to_string(), p("/")),
                    ("a".to_string(), p("/a")),
                    ("b".to_string(), p("/a/b")),
                ]
            );
        }
        if cfg!(windows) {
            assert_eq!(labels(r"C:\"), [("C:".to_string(), p(r"C:\"))]);
            assert_eq!(
                labels(r"C:\a\b"),
                [
                    ("C:".to_string(), p(r"C:\")),
                    ("a".to_string(), p(r"C:\a")),
                    ("b".to_string(), p(r"C:\a\b")),
                ]
            );
            assert_eq!(
                labels(r"\\s\sh\a"),
                [
                    (r"\\s\sh".to_string(), p(r"\\s\sh\")),
                    ("a".to_string(), p(r"\\s\sh\a")),
                ]
            );
            assert_eq!(
                labels(r"\\?\C:\a"),
                [
                    ("C:".to_string(), p(r"\\?\C:\")),
                    ("a".to_string(), p(r"\\?\C:\a")),
                ]
            );
            assert_eq!(
                labels("C:notes"),
                [
                    ("C:".to_string(), p("C:")),
                    ("notes".to_string(), p("C:notes")),
                ]
            );
        }
    }

    #[test]
    fn a_prefix_is_labelled_as_people_write_it() {
        assert_eq!(prefix_label(Prefix::Disk(b'C')), "C:");
        assert_eq!(prefix_label(Prefix::VerbatimDisk(b'd')), "d:");
        assert_eq!(
            prefix_label(Prefix::UNC(OsStr::new("s"), OsStr::new("sh"))),
            r"\\s\sh"
        );
        assert_eq!(
            prefix_label(Prefix::VerbatimUNC(OsStr::new("s"), OsStr::new("sh"))),
            r"\\s\sh"
        );
    }

    #[test]
    fn keys_fold_case_only_where_the_platform_does() {
        let mixed = Path::new(r"C:\Users\Brian\Ünïcode.TXT");
        assert_eq!(folded(mixed), p(r"c:\users\brian\ünïcode.txt"));
        assert_eq!(folded(Path::new("/home/Brian")), p("/home/brian"));
        if cfg!(windows) {
            assert_eq!(key(mixed).as_ref(), folded(mixed));
            assert_eq!(key(mixed), key(Path::new(r"c:\users\BRIAN\ünïcode.txt")));
            assert_eq!(into_key(mixed.to_path_buf()), folded(mixed));
        } else {
            assert_eq!(key(mixed).as_ref(), mixed, "the identity on Unix");
            assert!(matches!(key(mixed), Cow::Borrowed(_)), "and no copy");
            assert_eq!(into_key(mixed.to_path_buf()), mixed);
        }
    }

    #[test]
    fn unix_refuses_only_nul_and_the_slash() {
        assert!(name_is_valid_permissive(OsStr::new("a:b*c?.txt ")).is_ok());
        assert!(name_is_valid_permissive(OsStr::new("con")).is_ok());
        assert!(name_is_valid_permissive(OsStr::new("tab\there")).is_ok());
        assert!(name_is_valid_permissive(OsStr::new(r"back\slash")).is_ok());
        assert!(name_is_valid_permissive(OsStr::new("a/b")).is_err());
        assert!(name_is_valid_permissive(OsStr::new("a\0b")).is_err());
        if cfg!(unix) {
            assert!(name_is_valid(OsStr::new("con.txt")).is_ok());
            assert!(name_is_valid(OsStr::new("a:b")).is_ok());
        }
    }

    #[test]
    fn windows_refuses_its_characters_devices_and_trailing_dots() {
        let refused = |name: &str| name_is_valid_strict(OsStr::new(name)).is_err();
        for name in [
            "a:b",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a/b",
            r"a\b",
            "tab\there",
            "new\nline",
            "dot.",
            "space ",
            "con",
            "CON",
            "con.txt",
            "Con.tar.gz",
            "nul",
            "aux",
            "prn",
            "com1",
            "COM9",
            "lpt1.log",
            "com¹",
            "con .txt",
        ] {
            assert!(refused(name), "{name:?} should be refused");
        }
        for name in [
            "plain.txt",
            "with spaces.txt",
            "ünïcödé — 日本語 🎬.txt",
            "'quoted'.txt",
            "-leading-dash.txt",
            "%20already-encoded.txt",
            "console.txt",
            "com10",
            "com0",
            "lpt",
            ".hidden",
            "a.b.c",
        ] {
            assert!(!refused(name), "{name:?} should be allowed");
        }
        if cfg!(windows) {
            assert!(name_is_valid(OsStr::new("con.txt")).is_err());
        }
    }

    #[test]
    fn a_leading_tilde_is_home_and_the_rest_is_kept_as_typed() {
        assert_eq!(expand_home("/mnt/x"), "/mnt/x");
        assert_eq!(expand_home("sftp://h/srv"), "sftp://h/srv");
        let Some(home) = platform::dirs::home() else {
            assert_eq!(expand_home("~/Work"), "~/Work", "no home, no change");
            return;
        };
        let home = home.to_string_lossy();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/Work"), format!("{home}/Work"));
        assert_eq!(expand_home(r"~\Work"), format!(r"{home}\Work"));
    }

    #[test]
    fn slashes_replace_the_platform_separator_only() {
        assert_eq!(with_slashes("a/b"), "a/b");
        assert!(matches!(with_slashes("a/b"), Cow::Borrowed(_)));
        if cfg!(unix) {
            assert_eq!(with_slashes(r"a\b"), r"a\b", "a name's own backslash");
        }
        if cfg!(windows) {
            assert_eq!(with_slashes(r"..\a\b"), "../a/b");
            assert_eq!(with_slashes(r"C:\a/b"), "C:/a/b");
        }
    }

    #[test]
    fn display_takes_the_verbatim_prefix_off() {
        if cfg!(unix) {
            assert_eq!(display(Path::new("/a/b")), "/a/b");
            assert_eq!(display(Path::new(r"\\?\C:\a")), r"\\?\C:\a", "a name here");
        }
        if cfg!(windows) {
            assert_eq!(display(Path::new(r"\\?\C:\a")), r"C:\a");
            assert_eq!(display(Path::new(r"\\?\UNC\s\sh\a")), r"\\s\sh\a");
            assert_eq!(display(Path::new(r"C:\a")), r"C:\a");
            assert_eq!(display(Path::new(r"\\s\sh\a")), r"\\s\sh\a");
        }
    }
}
