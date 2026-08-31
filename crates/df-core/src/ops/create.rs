//! Create (`a`) and rename (`r` / `R`).
//!
//! Yazi's rule for `a`, kept exactly: a trailing `/` means "directory",
//! anything else means "file", and intermediate directories are created either
//! way — typing `a` then `notes/2026/plan.md` makes all three levels, which is
//! what the person typing it meant.
//!
//! Rename stays inside one directory. The input popup is anchored to the
//! hovered row and pre-selects the stem (PLAN §4.2); a name with a `/` in it is
//! a mistake, not a move, and is refused rather than quietly relocating a file
//! somewhere the user cannot see.

use std::path::{Path, PathBuf};

use crate::{DfError, Result};

use super::{exists, same_file};

/// What [`create`] made, for the journal and the toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Parent directories this call had to make. Undo removes them too — but
    /// only while they are still empty, since something else may have moved in.
    pub created_parents: Vec<PathBuf>,
}

/// Create a file or a directory. A trailing `/` means directory.
///
/// Refuses to touch an existing path: `a` is for making something new, and
/// truncating a file the user forgot about would be unrecoverable.
pub fn create(path: &Path) -> Result<Created> {
    use std::os::unix::ffi::OsStrExt;

    let raw = path.as_os_str().as_bytes();
    let is_dir = raw.last() == Some(&b'/');
    let path = if is_dir {
        // Trim the marker; `Path` keeps trailing slashes in its `OsStr`, and
        // `create_dir` does not mind them, but everything downstream compares
        // paths and `a/` must equal `a`.
        let trimmed = &raw[..raw.len() - 1];
        PathBuf::from(std::ffi::OsStr::from_bytes(trimmed))
    } else {
        path.to_path_buf()
    };

    if path.as_os_str().is_empty() {
        return Err(DfError::Op("no name given".to_string()));
    }
    if exists(&path) {
        return Err(DfError::Op(format!("{} already exists", path.display())));
    }

    let parent = path.parent().map(|p| p.to_path_buf());
    let mut created_parents = Vec::new();
    if let Some(parent) = parent {
        if !parent.as_os_str().is_empty() && !exists(&parent) {
            // Record which levels were missing, deepest last, so undo can peel
            // them off in the reverse order.
            let mut missing = Vec::new();
            let mut cursor = Some(parent.as_path());
            while let Some(dir) = cursor {
                if dir.as_os_str().is_empty() || exists(dir) {
                    break;
                }
                missing.push(dir.to_path_buf());
                cursor = dir.parent();
            }
            missing.reverse();
            std::fs::create_dir_all(&parent).map_err(|e| DfError::io(&parent, e))?;
            created_parents = missing;
        }
    }

    let made = if is_dir {
        std::fs::create_dir(&path).map_err(|e| DfError::io(&path, e))
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|_file| ())
            .map_err(|e| DfError::io(&path, e))
    };

    if let Err(e) = made {
        // Do not leave half a path behind after a failure.
        for dir in created_parents.iter().rev() {
            let _ignored = std::fs::remove_dir(dir);
        }
        return Err(e);
    }

    Ok(Created {
        path,
        is_dir,
        created_parents,
    })
}

/// Rename `from` to `to`, within one directory.
///
/// `force` is the only way past an existing name, and even then the old
/// destination is removed first — `rename(2)` will not replace a directory, and
/// half-replacing is worse than refusing. Renaming a file to its own name is a
/// no-op rather than an error, because that is what pressing Enter on an
/// unedited rename prompt means.
pub fn rename(from: &Path, to: &Path, force: bool) -> Result<()> {
    if !exists(from) {
        return Err(DfError::io(
            from,
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        ));
    }
    let name = to
        .file_name()
        .ok_or_else(|| DfError::Op(format!("{} is not a usable name", to.display())))?;
    if name.is_empty() {
        return Err(DfError::Op("no name given".to_string()));
    }

    let from_dir = from.parent().map(super::normalize);
    let to_dir = to.parent().map(super::normalize);
    if from_dir != to_dir {
        return Err(DfError::Op(format!(
            "rename stays in one directory: {} is not in {}",
            to.display(),
            from.parent().unwrap_or(Path::new("/")).display()
        )));
    }

    if same_file(from, to) {
        // The same file by a different spelling, including the unchanged name.
        return Ok(());
    }
    if exists(to) {
        if !force {
            return Err(DfError::io(
                to,
                std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already exists"),
            ));
        }
        super::delete::remove_tree_unchecked(to)?;
    }
    std::fs::rename(from, to).map_err(|e| DfError::io(from, e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::{gnarly_names, TempTree};

    #[test]
    fn creates_a_file() {
        let t = TempTree::new("create-file");
        let made = create(&t.join("notes.md")).unwrap();
        assert!(!made.is_dir);
        assert!(made.path.is_file());
        assert!(made.created_parents.is_empty());
    }

    #[test]
    fn a_trailing_slash_means_a_directory() {
        let t = TempTree::new("create-dir");
        let asked = t.join("stuff/");
        let made = create(&asked).unwrap();
        assert!(made.is_dir);
        assert!(made.path.is_dir());
        assert_eq!(made.path, t.join("stuff"), "the marker is trimmed");
    }

    #[test]
    fn creates_missing_parents_and_records_them() {
        let t = TempTree::new("create-parents");
        let made = create(&t.join("a/b/c/plan.md")).unwrap();
        assert!(made.path.is_file());
        assert_eq!(
            made.created_parents,
            vec![t.join("a"), t.join("a/b"), t.join("a/b/c")],
            "shallowest first"
        );
    }

    #[test]
    fn creates_gnarly_names() {
        let t = TempTree::new("create-gnarly");
        for name in gnarly_names() {
            let made = create(&t.join(&name)).unwrap();
            assert!(made.path.is_file(), "{name:?}");
        }
    }

    #[test]
    fn refuses_an_existing_path() {
        let t = TempTree::new("create-exists");
        let p = t.file("there.txt", b"contents");
        let err = create(&p).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&p).unwrap(), b"contents");
    }

    #[test]
    fn rename_moves_within_the_directory() {
        let t = TempTree::new("rename");
        let from = t.file("old.txt", b"x");
        let to = t.join("new — ünïcödé.txt");
        rename(&from, &to, false).unwrap();
        assert!(!super::exists(&from));
        assert_eq!(std::fs::read(&to).unwrap(), b"x");
    }

    #[test]
    fn rename_refuses_to_leave_the_directory() {
        let t = TempTree::new("rename-escape");
        let from = t.file("a/old.txt", b"x");
        t.dir("b");
        let err = rename(&from, &t.join("b/old.txt"), true).unwrap_err();
        assert!(err.to_string().contains("stays in one directory"), "{err}");
        assert!(from.is_file());
    }

    #[test]
    fn rename_will_not_clobber_without_force() {
        let t = TempTree::new("rename-clobber");
        let from = t.file("a.txt", b"new");
        let to = t.file("b.txt", b"old");
        let err = rename(&from, &to, false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&to).unwrap(), b"old");

        rename(&from, &to, true).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!super::exists(&from));
    }

    #[test]
    fn renaming_to_the_same_name_is_a_no_op() {
        let t = TempTree::new("rename-same");
        let p = t.file("a.txt", b"x");
        rename(&p, &p, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
    }

    #[test]
    fn rename_reports_a_missing_source() {
        let t = TempTree::new("rename-missing");
        let err = rename(&t.join("nope"), &t.join("also-nope"), false).unwrap_err();
        assert!(err.to_string().contains("no such file"), "{err}");
    }
}
