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

/// Create a file or a directory. A trailing separator means directory — `/`,
/// or on Windows `\` as well.
///
/// Refuses to touch an existing path: `a` is for making something new, and
/// truncating a file the user forgot about would be unrecoverable.
pub fn create(path: &Path) -> Result<Created> {
    let is_dir = crate::path::has_trailing_separator(path.as_os_str());
    let path = if is_dir {
        // Trim the marker; `Path` keeps trailing slashes in its `OsStr`, and
        // `create_dir` does not mind them, but everything downstream compares
        // paths and `a/` must equal `a`. One separator, and one byte: every
        // separator is ASCII.
        let raw = crate::platform::os::as_bytes(path.as_os_str())?;
        PathBuf::from(crate::platform::os::from_bytes(&raw[..raw.len() - 1])?)
    } else {
        path.to_path_buf()
    };

    if path.as_os_str().is_empty() {
        return Err(DfError::Op("no name given".to_string()));
    }
    if exists(&path) {
        return Err(DfError::Op(format!("{} already exists", path.display())));
    }
    // Every name this makes, the leaf and each missing parent, has to be one
    // the platform takes — refused up front, with the reason, rather than
    // half made (Windows: `a:b`, `con`, a trailing dot).
    for made in path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !exists(p))
    {
        if let Some(name) = made.file_name() {
            valid(name)?;
        }
    }

    let created_parents = make_parents(&path)?;

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
        remove_parents(&created_parents);
        return Err(e);
    }

    Ok(Created {
        path,
        is_dir,
        created_parents,
    })
}

/// Make the directories `path` needs above it, and say which were missing —
/// shallowest first, so an undo or a failure can peel them off in reverse.
///
/// `a`'s rule, shared with anything else that takes a typed name with slashes
/// in it: an archive written as `out/photos.zip` makes `out/` the same way
/// `a` makes it for `out/notes.md`.
pub fn make_parents(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    if parent.as_os_str().is_empty() || exists(parent) {
        return Ok(Vec::new());
    }
    let mut missing = Vec::new();
    let mut cursor = Some(parent);
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() || exists(dir) {
            break;
        }
        missing.push(dir.to_path_buf());
        cursor = dir.parent();
    }
    missing.reverse();
    std::fs::create_dir_all(parent).map_err(|e| DfError::io(parent, e))?;
    Ok(missing)
}

/// Take back what [`make_parents`] made, deepest first, stopping at the first
/// one that is not empty — something else has moved in, and it stays.
pub fn remove_parents(made: &[PathBuf]) {
    for dir in made.iter().rev() {
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
}

/// Rename `from` to `to`, within one directory.
///
/// `force` is the only way past an existing name, and even then the old
/// destination is removed first — `rename(2)` will not replace a directory, and
/// half-replacing is worse than refusing. Renaming a file to its own name is a
/// no-op rather than an error, because that is what pressing Enter on an
/// unedited rename prompt means.
///
/// On a volume that folds case (Windows, the default macOS one, a FAT card)
/// `Foo` and `foo` are one file, and renaming one to the other is how a
/// person fixes the case of a name — so it is done, by way of a temporary
/// name, rather than taken for the unchanged name.
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
    valid(name)?;

    // One directory however it is spelled: keyed, so `C:\Dir` and `c:\dir`
    // are one on Windows. On Unix the key is the path itself.
    let from_dir = from.parent().map(super::normalize);
    let to_dir = to.parent().map(super::normalize);
    if from_dir.as_deref().map(crate::path::key) != to_dir.as_deref().map(crate::path::key) {
        return Err(DfError::Op(format!(
            "rename stays in one directory: {} is not in {}",
            to.display(),
            from.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| crate::path::root_of(from))
                .display()
        )));
    }

    if same_file(from, to) {
        if is_case_change(from, to) {
            return rename_by_way_of_a_temporary_name(from, to);
        }
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

/// `name`, if the platform takes it ([`crate::path::name_is_valid`]), or the
/// refusal that says why.
fn valid(name: &std::ffi::OsStr) -> Result<()> {
    crate::path::name_is_valid(name).map_err(|why| {
        DfError::Op(format!(
            "\"{}\" cannot be a name here: {why}",
            name.to_string_lossy()
        ))
    })
}

/// Whether `to` is `from` in another case, reached only because the volume
/// folds case: the names differ, they are equal once lowercased, and the
/// directory has no entry spelled as `to` is. On a volume that does not fold
/// case — Linux's — two such names that are one file are two hard links, both
/// listed, and renaming one onto the other stays the no-op it was.
fn is_case_change(from: &Path, to: &Path) -> bool {
    let (Some(old), Some(new)) = (
        from.file_name().and_then(|n| n.to_str()),
        to.file_name().and_then(|n| n.to_str()),
    ) else {
        return false;
    };
    if old == new || old.to_lowercase() != new.to_lowercase() {
        return false;
    }
    let Some(dir) = to.parent() else {
        return false;
    };
    let listed = |dir: &Path| -> std::io::Result<bool> {
        for entry in std::fs::read_dir(dir)? {
            if entry?.file_name() == std::ffi::OsStr::new(new) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    matches!(listed(dir), Ok(false))
}

/// `from` to `to` in two steps, through a free `.df-tmp-` name beside them:
/// a rename that only changes case is a no-op to some file systems. When the
/// second step fails the first is taken back, so the file keeps its old name.
fn rename_by_way_of_a_temporary_name(from: &Path, to: &Path) -> Result<()> {
    let dir = from.parent().unwrap_or(Path::new("."));
    // As many tries as a copy's temporary names get (`copy.rs`).
    const ATTEMPTS: u32 = 16;
    let temp = (0..ATTEMPTS)
        .map(|n| {
            dir.join(format!(
                "{}rename-{}-{n}",
                super::copy::TEMP_PREFIX,
                std::process::id()
            ))
        })
        .find(|candidate| !exists(candidate))
        .ok_or_else(|| DfError::Op(format!("no free temporary name in {}", dir.display())))?;
    std::fs::rename(from, &temp).map_err(|e| DfError::io(from, e))?;
    if let Err(e) = std::fs::rename(&temp, to) {
        let _ignored = std::fs::rename(&temp, from);
        return Err(DfError::io(to, e));
    }
    Ok(())
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

    /// The platform's own separator is the marker too: `\` on Windows. On
    /// Unix a `\` is part of a name, so `stuff\` there is a file.
    #[test]
    fn a_trailing_platform_separator_means_a_directory() {
        let t = TempTree::new("create-dir-sep");
        let asked = t.join(format!("stuff{}", std::path::MAIN_SEPARATOR));
        let made = create(&asked).unwrap();
        assert!(made.is_dir && made.path.is_dir());
        assert_eq!(made.path, t.join("stuff"));
        if cfg!(unix) {
            let made = create(&t.join("back\\")).unwrap();
            assert!(!made.is_dir, "a backslash is a name's on Unix");
        }
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

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Where the volume folds case, `Foo.txt` renamed to `foo.txt` is the
    /// same file under a new spelling — done, not taken for the unchanged
    /// name. Skipped on a volume that keeps case (Linux's).
    #[test]
    fn a_case_only_rename_changes_the_case_where_the_volume_folds_it() {
        let t = TempTree::new("rename-case");
        if !crate::test_support::folds_case(t.path()) {
            eprintln!("skipping: the temp volume keeps case");
            return;
        }
        let from = t.file("Foo.txt", b"x");
        rename(&from, &t.join("foo.txt"), false).unwrap();
        assert_eq!(names_in(t.path()), ["foo.txt"]);
        assert_eq!(std::fs::read(t.join("foo.txt")).unwrap(), b"x");
        // And a folder, back again.
        t.dir("Dir/inside");
        rename(&t.join("Dir"), &t.join("dir"), false).unwrap();
        assert!(names_in(t.path()).contains(&"dir".to_string()));
        assert!(t.join("dir/inside").is_dir());
    }

    /// Where the volume keeps case, `Foo.txt` and `foo.txt` that are one file
    /// are two hard links, and renaming one onto the other is the no-op it
    /// always was: both names stay.
    #[test]
    fn a_rename_onto_a_hard_link_in_another_case_is_a_no_op() {
        let t = TempTree::new("rename-case-link");
        if crate::test_support::folds_case(t.path()) {
            eprintln!("skipping: the temp volume folds case");
            return;
        }
        let from = t.file("Foo.txt", b"x");
        std::fs::hard_link(&from, t.join("foo.txt")).unwrap();
        rename(&from, &t.join("foo.txt"), false).unwrap();
        assert_eq!(names_in(t.path()), ["Foo.txt", "foo.txt"]);
    }

    /// A name Windows will not make is refused there before anything is
    /// made, with the reason; the same names are ordinary on Unix.
    #[test]
    fn a_name_the_platform_refuses_is_refused_with_its_reason() {
        let t = TempTree::new("create-invalid");
        for name in ["x:y.txt", "con.txt", "trailing.", "sub/what?/inner.txt"] {
            let made = create(&t.join(name));
            if cfg!(windows) {
                let err = made.unwrap_err().to_string();
                assert!(err.contains("cannot be a name here"), "{name}: {err}");
            } else {
                assert!(made.is_ok(), "{name}: {made:?}");
            }
        }
        assert!(!t.join("sub").exists() || cfg!(unix), "nothing half made");

        let from = t.file("ok.txt", b"x");
        let renamed = rename(&from, &t.join("aux.txt"), false);
        assert_eq!(renamed.is_err(), cfg!(windows), "{renamed:?}");
    }

    #[test]
    fn rename_reports_a_missing_source() {
        let t = TempTree::new("rename-missing");
        let err = rename(&t.join("nope"), &t.join("also-nope"), false).unwrap_err();
        assert!(err.to_string().contains("no such file"), "{err}");
    }
}
