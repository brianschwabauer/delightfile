//! A file dialog's type filters: "Images", "PDF documents", the list a
//! browser's `<input accept="image/*">` hands the file-chooser portal.
//!
//! Here rather than in df-app, where the portal's request is read, because a
//! filter is applied where the hidden toggle is — inside [`super::DirState`]'s
//! view — so the cursor, the counts, select-all and the position counter only
//! ever see rows that passed it, and a rescan or a new directory is filtered by
//! the same rebuild that sorts it.
//!
//! Two pattern kinds because the portal has two. A glob is matched against the
//! file's name, case-insensitively — a picker that hid `PHOTO.JPG` from a
//! `*.jpg` filter would be wrong far more often than right. A MIME pattern is
//! matched against the type the name implies ([`super::mime::hint_for_name`]):
//! exactly (`image/png`), or a whole family (`image/*`). The hint is a guess
//! from the extension and is used as one; sniffing every file in a directory to
//! decide whether to *show* it would be the open-and-read per entry the hint
//! exists to avoid.

use super::entry::Entry;
use super::mime::{hint_for_name, UNKNOWN_MIME};

/// One named file-type filter, as the file-chooser portal describes it: a
/// label for the menu and the patterns a file has to match to be shown.
///
/// A file passes when any one pattern matches. A directory always passes —
/// a filter that hid folders would leave nothing to walk to the files with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeFilter {
    pub name: String,
    /// Shell globs against the file name: `*`, `?` and `[...]`.
    pub globs: Vec<String>,
    /// `type/subtype`, or `type/*` for the family.
    pub mimes: Vec<String>,
}

impl TypeFilter {
    /// Whether `entry` is shown while this filter is on.
    ///
    /// `is_dir` includes a symlink to a directory, for the reason it does
    /// everywhere else: `→` enters it, so it is a way to the files.
    pub fn admits(&self, entry: &Entry) -> bool {
        entry.is_dir() || self.matches(&entry.name)
    }

    /// Whether a *file* called `name` passes.
    pub fn matches(&self, name: &str) -> bool {
        if self.globs.iter().any(|glob| glob_matches(glob, name)) {
            return true;
        }
        if self.mimes.is_empty() {
            return false;
        }
        let mime = hint_for_name(name);
        self.mimes.iter().any(|pattern| mime_matches(pattern, mime))
    }
}

/// A shell glob against a file name, case-insensitively.
///
/// `*` is any run of characters, `?` one character, and `[...]` one of a set
/// (`[abc]`, `[a-z]`, negated by a leading `!` or `^`; a `]` straight after
/// the opening bracket is a member rather than the end). A `[` that is never
/// closed is a literal bracket, which is what `fnmatch` does with it. Both
/// sides are folded to lowercase first, so `*.JPG`, `*.jpg` and `[A-Z]*` mean
/// what a person typing them meant.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    wildcard(&pattern, &name)
}

/// A MIME pattern against a type: exact, or `family/*`. The comparison
/// ignores ASCII case, as MIME types do.
///
/// A name that implies no type fails every pattern — `application/*` must not
/// admit `notes.xyz` on the strength of the octet-stream fallback, which is an
/// admission of not knowing rather than a type.
pub fn mime_matches(pattern: &str, mime: &str) -> bool {
    if mime == UNKNOWN_MIME {
        return false;
    }
    let pattern = pattern.trim();
    match pattern.split_once('/') {
        Some((family, "*")) => mime
            .split_once('/')
            .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case(family)),
        _ => pattern.eq_ignore_ascii_case(mime),
    }
}

/// The match itself: a walk over both with one remembered `*` to fall back
/// to.
///
/// Only the most recent `*` is ever retried. An earlier one could only take
/// more characters for the later one to give back, so going back further
/// never finds a match this misses — which keeps a name against any pattern
/// linear-ish rather than exponential in the stars.
fn wildcard(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where to resume after the last `*`: the pattern just past it, and the
    // name position it has swallowed up to.
    let mut resume: Option<(usize, usize)> = None;
    while n < name.len() {
        if pattern.get(p) == Some(&'*') {
            p += 1;
            resume = Some((p, n));
            continue;
        }
        if let Some(taken) = pattern.get(p..).and_then(|rest| token(rest, name[n])) {
            p += taken;
            n += 1;
            continue;
        }
        let Some((after, swallowed)) = resume else {
            return false;
        };
        // The `*` takes one more character and the rest of the pattern tries
        // again from there.
        p = after;
        n = swallowed + 1;
        resume = Some((after, n));
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Whether the single-character token at the front of `pattern` matches `c`,
/// and how many pattern characters it spans when it does. Never called on a
/// `*`, which is [`wildcard`]'s business.
fn token(pattern: &[char], c: char) -> Option<usize> {
    match pattern.first()? {
        '?' => Some(1),
        '[' => match class(pattern, c) {
            Some((true, len)) => Some(len),
            Some((false, _)) => None,
            None => (c == '[').then_some(1),
        },
        literal => (*literal == c).then_some(1),
    }
}

/// A `[...]` at the front of `pattern`: whether `c` is in it, and how long the
/// class is. `None` when it is never closed.
fn class(pattern: &[char], c: char) -> Option<(bool, usize)> {
    let mut i = 1;
    let negated = matches!(pattern.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let first = i;
    let mut hit = false;
    loop {
        let low = *pattern.get(i)?;
        if low == ']' && i > first {
            return Some((hit != negated, i + 1));
        }
        match (pattern.get(i + 1), pattern.get(i + 2)) {
            (Some('-'), Some(&high)) if high != ']' => {
                hit |= low <= c && c <= high;
                i += 3;
            }
            _ => {
                hit |= low == c;
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Kind, LinkTarget};
    use std::path::PathBuf;

    fn filter(globs: &[&str], mimes: &[&str]) -> TypeFilter {
        TypeFilter {
            name: "Test".to_string(),
            globs: globs.iter().map(|g| g.to_string()).collect(),
            mimes: mimes.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/fixture").join(name),
            kind,
            len: 0,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            is_hidden: name.starts_with('.'),
            mime: hint_for_name(name),
            file_kind: crate::fs::classify(kind, name, hint_for_name(name), 0o644),
        }
    }

    /// The glob table: the three metacharacters, their edges, and the case
    /// fold that makes `*.jpg` find a camera's `IMG_0001.JPG`.
    #[test]
    fn globs_match_names_case_insensitively() {
        let table: &[(&str, &str, bool)] = &[
            ("*.png", "photo.png", true),
            ("*.png", "photo.PNG", true),
            ("*.PNG", "photo.png", true),
            ("*.jpg", "IMG_0001.JPG", true),
            ("*.png", "photo.png.bak", false),
            ("*.png", "png", false),
            ("*", "anything at all", true),
            ("*", "", true),
            ("", "", true),
            ("", "a", false),
            // `?` is exactly one character.
            ("?.txt", "a.txt", true),
            ("?.txt", "ab.txt", false),
            ("??.txt", "ab.txt", true),
            // Several stars, and a star that has to give characters back.
            ("*a*b*", "xxaxxbxx", true),
            ("*a*b*", "xxbxxaxx", false),
            ("a*a*a", "aaa", true),
            ("a*a*a", "aa", false),
            ("*.tar.*", "backup.tar.gz", true),
            // Classes, ranges and negation.
            ("*.[ch]", "main.c", true),
            ("*.[ch]", "main.h", true),
            ("*.[ch]", "main.o", false),
            ("*.[CH]", "main.c", true),
            ("[a-c]*", "banana", true),
            ("[a-c]*", "Banana", true),
            ("[a-c]*", "date", false),
            ("[!a-c]*", "date", true),
            ("[!a-c]*", "apple", false),
            ("[^a-c]*", "apple", false),
            ("file[0-9].txt", "file7.txt", true),
            ("file[0-9].txt", "filex.txt", false),
            // `]` first in a class is a member, and `-` at either end is one.
            ("[]]", "]", true),
            ("[a-]", "-", true),
            ("[-a]", "-", true),
            // An unclosed bracket is a literal one.
            ("[abc", "[abc", true),
            ("[abc", "a", false),
        ];
        for (pattern, name, want) in table {
            assert_eq!(
                glob_matches(pattern, name),
                *want,
                "{pattern:?} against {name:?}"
            );
        }
    }

    /// MIME patterns go through the name's hint: exact, a family, and nothing
    /// at all for a name that implies no type.
    #[test]
    fn mime_patterns_match_the_type_the_name_implies() {
        assert!(mime_matches("image/png", "image/png"));
        assert!(mime_matches("IMAGE/PNG", "image/png"));
        assert!(mime_matches("image/*", "image/jpeg"));
        assert!(mime_matches("Image/*", "image/jpeg"));
        assert!(!mime_matches("image/*", "video/mp4"));
        assert!(!mime_matches("image/png", "image/jpeg"));
        // Not a prefix match: `image/*` is the family, not "starts with".
        assert!(!mime_matches("image/*", "imagery/x"));
        // No hint: every pattern fails, the family one included.
        assert!(!mime_matches("application/*", UNKNOWN_MIME));
        assert!(!mime_matches(UNKNOWN_MIME, UNKNOWN_MIME));

        let images = filter(&[], &["image/*"]);
        assert!(images.matches("photo.jpg"));
        assert!(images.matches("SCAN.TIFF"));
        assert!(images.matches("drawing.svg"));
        assert!(!images.matches("clip.mp4"));
        assert!(!images.matches("notes.xyz"), "no hint, no match");
        assert!(!images.matches("README"), "a whole name is text, not image");
        let pdf = filter(&[], &["application/pdf"]);
        assert!(pdf.matches("paper.PDF"));
        assert!(!pdf.matches("paper.pdf.part"));
    }

    /// A filter passes a file on any one pattern, globs and MIME types alike,
    /// and a filter with no patterns passes no file.
    #[test]
    fn any_pattern_admits_a_file() {
        let both = filter(&["*.webp"], &["image/png"]);
        assert!(both.matches("a.webp"));
        assert!(both.matches("a.png"));
        assert!(!both.matches("a.jpg"));
        assert!(!filter(&[], &[]).matches("a.png"));
    }

    /// Directories, and links to them, always pass: they are how the files
    /// that do are reached. A link to a file is judged by its name, and so is
    /// a broken one.
    #[test]
    fn directories_always_pass() {
        let images = filter(&["*.png"], &[]);
        assert!(images.admits(&entry("Screenshots", Kind::Dir)));
        assert!(images.admits(&entry(
            "linked",
            Kind::Symlink {
                target: Some(LinkTarget::Dir)
            }
        )));
        assert!(images.admits(&entry("a.png", Kind::File)));
        assert!(!images.admits(&entry("a.txt", Kind::File)));
        assert!(images.admits(&entry(
            "link.png",
            Kind::Symlink {
                target: Some(LinkTarget::File)
            }
        )));
        assert!(!images.admits(&entry(
            "link.txt",
            Kind::Symlink {
                target: Some(LinkTarget::File)
            }
        )));
        assert!(images.admits(&entry("gone.png", Kind::Symlink { target: None })));
    }
}
