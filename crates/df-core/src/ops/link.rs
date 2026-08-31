//! Symlinks and hardlinks — yazi's `-`, `_` and `Ctrl+-`.
//!
//! Absolute links are trivial. Relative ones are the interesting half: the link
//! text has to be computed *between two arbitrary paths*, which means walking
//! up from the link's own directory with `..` until the two paths share a
//! prefix. Getting it wrong produces a link that resolves somewhere else
//! entirely, or that breaks the moment the tree is moved — which is the whole
//! reason to want a relative link.

use std::path::{Component, Path, PathBuf};

use crate::{DfError, Result};

use super::{exists, normalize};

/// Which flavour of link `-` / `_` make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `-`: the link text is the target's absolute path.
    Absolute,
    /// `_`: the link text is the target relative to the link's own directory,
    /// so moving the pair together keeps it valid.
    Relative,
}

/// The relative path from directory `from_dir` to `to`.
///
/// Pure and lexical over normalized paths — no disk access, no symlink
/// resolution (resolving would defeat the point: a relative link *through* a
/// symlinked parent is usually exactly what was asked for). Returns `.` when
/// the two are the same place.
pub fn relative_to(from_dir: &Path, to: &Path) -> PathBuf {
    let from = normalize(from_dir);
    let to = normalize(to);

    let mut f = from.components().peekable();
    let mut t = to.components().peekable();
    // Drop the shared prefix.
    while let (Some(a), Some(b)) = (f.peek(), t.peek()) {
        if a == b {
            f.next();
            t.next();
        } else {
            break;
        }
    }

    let mut out = PathBuf::new();
    for comp in f {
        // Every component of `from` that is not shared is one level to climb.
        if matches!(comp, Component::Normal(_)) {
            out.push("..");
        }
    }
    for comp in t {
        out.push(comp.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Create a symbolic link at `link` pointing at `target`.
///
/// The target is not required to exist: a link to something not yet there is a
/// legitimate thing to make, and refusing would be a surprise. Returns the link
/// text actually written, which the journal keeps so undo can prove it is
/// unlinking the link it made and not a newer one.
pub fn symlink(target: &Path, link: &Path, kind: LinkKind) -> Result<PathBuf> {
    if exists(link) {
        return Err(DfError::Op(format!(
            "{} already exists",
            link.display()
        )));
    }
    let text = match kind {
        LinkKind::Absolute => normalize(target),
        LinkKind::Relative => {
            let dir = link
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("/"));
            relative_to(&dir, target)
        }
    };
    std::os::unix::fs::symlink(&text, link).map_err(|e| DfError::io(link, e))?;
    Ok(text)
}

/// Create a hard link at `link` to the file `target`.
///
/// Directories cannot be hard linked (the kernel refuses, and rightly), and a
/// hard link to a symlink follows it — so both are turned into errors the user
/// can read rather than an `EPERM` they cannot.
pub fn hardlink(target: &Path, link: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(target).map_err(|e| DfError::io(target, e))?;
    if meta.is_dir() {
        return Err(DfError::Op(format!(
            "{} is a directory: hard links to directories are not possible",
            target.display()
        )));
    }
    if meta.is_symlink() {
        return Err(DfError::Op(format!(
            "{} is a symlink: hard link its target instead",
            target.display()
        )));
    }
    if exists(link) {
        return Err(DfError::Op(format!("{} already exists", link.display())));
    }
    std::fs::hard_link(target, link).map_err(|e| DfError::io(link, e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn relative_between_siblings() {
        assert_eq!(
            relative_to(Path::new("/a/b"), Path::new("/a/c/file")),
            Path::new("../c/file")
        );
    }

    #[test]
    fn relative_into_a_child() {
        assert_eq!(
            relative_to(Path::new("/a"), Path::new("/a/b/c")),
            Path::new("b/c")
        );
    }

    #[test]
    fn relative_up_several_levels() {
        assert_eq!(
            relative_to(Path::new("/a/b/c/d"), Path::new("/a/x")),
            Path::new("../../../x")
        );
    }

    #[test]
    fn relative_to_the_same_place_is_dot() {
        assert_eq!(relative_to(Path::new("/a/b"), Path::new("/a/b")), Path::new("."));
    }

    #[test]
    fn relative_across_the_root() {
        assert_eq!(
            relative_to(Path::new("/a/b"), Path::new("/x/y")),
            Path::new("../../x/y")
        );
    }

    #[test]
    fn relative_ignores_a_shared_name_prefix() {
        assert_eq!(
            relative_to(Path::new("/a/bc"), Path::new("/a/bcd/f")),
            Path::new("../bcd/f")
        );
    }

    #[test]
    fn relative_handles_gnarly_names() {
        assert_eq!(
            relative_to(Path::new("/a/with space"), Path::new("/a/ünï 🎬/x\ny")),
            Path::new("../ünï 🎬/x\ny")
        );
    }

    #[test]
    fn absolute_symlink_points_at_the_absolute_path() {
        let t = TempTree::new("link-abs");
        let target = t.file("dir/target.txt", b"hi");
        let link = t.join("link");
        let text = symlink(&target, &link, LinkKind::Absolute).unwrap();
        assert_eq!(text, normalize(&target));
        assert_eq!(std::fs::read_link(&link).unwrap(), normalize(&target));
        assert_eq!(std::fs::read(&link).unwrap(), b"hi");
    }

    #[test]
    fn relative_symlink_resolves_and_survives_a_move() {
        let t = TempTree::new("link-rel");
        let target = t.file("a/target.txt", b"hi");
        t.dir("b");
        let link = t.join("b/link");
        let text = symlink(&target, &link, LinkKind::Relative).unwrap();
        assert_eq!(text, Path::new("../a/target.txt"));
        assert_eq!(std::fs::read(&link).unwrap(), b"hi");

        // Move the whole pair: a relative link still resolves.
        let moved = t.join("moved");
        std::fs::create_dir(&moved).unwrap();
        std::fs::rename(t.join("a"), moved.join("a")).unwrap();
        std::fs::rename(t.join("b"), moved.join("b")).unwrap();
        assert_eq!(std::fs::read(moved.join("b/link")).unwrap(), b"hi");
    }

    #[test]
    fn symlink_to_a_missing_target_is_allowed() {
        let t = TempTree::new("link-missing");
        let link = t.join("dangling");
        symlink(&t.join("not-here"), &link, LinkKind::Absolute).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[test]
    fn symlink_refuses_to_clobber() {
        let t = TempTree::new("link-clobber");
        let target = t.file("t", b"x");
        let link = t.file("l", b"existing");
        let err = symlink(&target, &link, LinkKind::Absolute).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&link).unwrap(), b"existing");
    }

    #[test]
    fn hardlink_shares_the_inode() {
        let t = TempTree::new("hardlink");
        let target = t.file("t", b"x");
        let link = t.join("l");
        hardlink(&target, &link).unwrap();
        let a = std::fs::metadata(&target).unwrap();
        let b = std::fs::metadata(&link).unwrap();
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
        assert_eq!(b.nlink(), 2);
    }

    #[test]
    fn hardlink_refuses_directories_and_symlinks() {
        let t = TempTree::new("hardlink-refuse");
        let dir = t.dir("d");
        let err = hardlink(&dir, &t.join("dl")).unwrap_err();
        assert!(err.to_string().contains("directory"), "{err}");

        let target = t.file("t", b"x");
        let sym = t.join("s");
        symlink(&target, &sym, LinkKind::Absolute).unwrap();
        let err = hardlink(&sym, &t.join("hl")).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }
}
