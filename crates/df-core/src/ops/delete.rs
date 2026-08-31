//! Permanent delete — the one operation with no inverse.
//!
//! `d` goes to the trash and is always undoable (PLAN §5); `D` is this, and it
//! is deliberately *not* journalled, because a journal entry that cannot be
//! replayed is a promise the program cannot keep. The confirm dialog in df-app
//! is the last word, and the rails below are the ones that hold even if the
//! dialog is bypassed: a bug in a keybinding must not be able to delete `$HOME`.
//!
//! ## The rails
//!
//! [`check_deletable`] refuses, in order:
//!
//! 1. the filesystem root `/`;
//! 2. `$HOME` itself, and anything above it (`/home`), because "delete the
//!    thing my cursor is on" should never mean "delete every user";
//! 3. any path at or above the current working directory — the directory the
//!    program is standing in. This is the "root sanity" rail: deleting the
//!    ground you are standing on leaves the process with a cwd that no longer
//!    exists, and the paths it is holding are suddenly meaningless.
//!
//! The rails are lexical, over normalized paths, and take cwd and home as
//! arguments so they are testable without touching the real ones.

use std::path::Path;

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::{is_ancestor, normalize};

/// Would deleting `path` be reckless? Errors say which rail was hit.
pub fn check_deletable(path: &Path, cwd: &Path, home: Option<&Path>) -> Result<()> {
    let target = normalize(path);

    if target == Path::new("/") {
        return Err(DfError::Op(
            "refusing to delete the filesystem root".to_string(),
        ));
    }
    if let Some(home) = home {
        let home = normalize(home);
        if is_ancestor(&target, &home) {
            return Err(DfError::Op(format!(
                "refusing to delete {}: it is your home directory or contains it",
                target.display()
            )));
        }
    }
    if is_ancestor(&target, &normalize(cwd)) {
        return Err(DfError::Op(format!(
            "refusing to delete {}: it is the directory delightfile is in, or contains it",
            target.display()
        )));
    }
    Ok(())
}

/// The rails, resolved against the real cwd and `$HOME`.
pub fn check_deletable_here(path: &Path) -> Result<()> {
    // A process whose cwd has been deleted out from under it still deserves the
    // other rails, so an unreadable cwd falls back to `/` — which makes the cwd
    // rail refuse everything rather than nothing.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    check_deletable(path, &cwd, home.as_deref())
}

/// Delete `path` and everything under it, for good.
///
/// Cancellable and progress-reporting like every other operation, which matters
/// on a 200k-file tree. Cancelling leaves the tree partly deleted — there is no
/// way to put back what has already gone, and stopping is still better than
/// carrying on with an operation the user has taken back.
pub fn delete_permanent(path: &Path, ctx: &TaskCtx) -> Result<()> {
    check_deletable_here(path)?;
    remove_tree(path, ctx)
}

/// The recursive removal itself, past the rails.
///
/// Public because a cross-device move ends with it, having already established
/// that the destination holds a verified copy — the rails do not apply to a
/// path the user asked to *move*.
pub fn remove_tree(path: &Path, ctx: &TaskCtx) -> Result<()> {
    ctx.checkpoint()?;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        // Already gone is the state we wanted.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DfError::io(path, e)),
    };

    // A symlink to a directory is unlinked, never descended into: deleting a
    // link to ~/Pictures must not delete ~/Pictures.
    if meta.is_dir() && !meta.is_symlink() {
        for entry in std::fs::read_dir(path).map_err(|e| DfError::io(path, e))? {
            let entry = entry.map_err(|e| DfError::io(path, e))?;
            remove_tree(&entry.path(), ctx)?;
        }
        std::fs::remove_dir(path).map_err(|e| DfError::io(path, e))?;
        ctx.advance(0, 1);
        return Ok(());
    }

    let len = if meta.is_symlink() { 0 } else { meta.len() };
    std::fs::remove_file(path).map_err(|e| DfError::io(path, e))?;
    ctx.advance(len, 1);
    Ok(())
}

/// Remove a tree with no cancellation and no rails.
///
/// Only for cleaning up something *this program just created* — a partial copy,
/// a destination the user chose to overwrite. Never reachable from a keystroke.
pub(crate) fn remove_tree_unchecked(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DfError::io(path, e)),
    };
    if meta.is_dir() && !meta.is_symlink() {
        std::fs::remove_dir_all(path).map_err(|e| DfError::io(path, e))
    } else {
        std::fs::remove_file(path).map_err(|e| DfError::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::exists;
    use crate::ops::fixture::{gnarly_names, TempTree};

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    #[test]
    fn deletes_a_tree_of_gnarly_names() {
        let t = TempTree::new("delete");
        let victim = t.dir("victim");
        for name in gnarly_names() {
            std::fs::write(victim.join(&name), b"x").unwrap();
        }
        std::fs::create_dir(victim.join("sub")).unwrap();
        std::fs::write(victim.join("sub/deep"), b"y").unwrap();
        remove_tree(&victim, &ctx()).unwrap();
        assert!(!exists(&victim));
    }

    #[test]
    fn never_descends_a_symlinked_directory() {
        let t = TempTree::new("delete-symlink");
        let real = t.dir("real");
        std::fs::write(real.join("precious"), b"keep").unwrap();
        let victim = t.dir("victim");
        std::os::unix::fs::symlink(&real, victim.join("link")).unwrap();

        remove_tree(&victim, &ctx()).unwrap();
        assert!(!exists(&victim));
        assert!(real.join("precious").is_file(), "the target survives");
    }

    #[test]
    fn removes_a_broken_symlink() {
        let t = TempTree::new("delete-broken");
        let link = t.symlink(t.join("nowhere"), "broken");
        remove_tree(&link, &ctx()).unwrap();
        assert!(!exists(&link));
    }

    #[test]
    fn deleting_a_missing_path_is_not_an_error() {
        let t = TempTree::new("delete-missing");
        remove_tree(&t.join("never-existed"), &ctx()).unwrap();
    }

    #[test]
    fn cancel_stops_the_walk() {
        let t = TempTree::new("delete-cancel");
        let victim = t.dir("victim");
        for i in 0..50 {
            std::fs::write(victim.join(format!("f{i}")), b"x").unwrap();
        }
        let ctx = TaskCtx::detached();
        ctx.flags().cancel();
        assert!(matches!(
            remove_tree(&victim, &ctx),
            Err(DfError::Cancelled)
        ));
        assert!(exists(&victim), "cancelled before it started");
    }

    #[test]
    fn rail_refuses_the_root() {
        let err = check_deletable(
            Path::new("/"),
            Path::new("/home/someone/work"),
            Some(Path::new("/home/someone")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("filesystem root"), "{err}");
    }

    #[test]
    fn rail_refuses_home_and_its_ancestors() {
        let home = Path::new("/home/someone");
        let cwd = Path::new("/home/someone/work");
        let err = check_deletable(home, cwd, Some(home)).unwrap_err();
        assert!(err.to_string().contains("home directory"), "{err}");
        let err = check_deletable(Path::new("/home"), cwd, Some(home)).unwrap_err();
        assert!(err.to_string().contains("home directory"), "{err}");
        // Something inside home is perfectly deletable.
        check_deletable(Path::new("/home/someone/junk"), cwd, Some(home)).unwrap();
    }

    #[test]
    fn rail_refuses_a_prefix_of_the_cwd() {
        let home = Path::new("/home/someone");
        let cwd = Path::new("/home/someone/work/delightfile");
        let err = check_deletable(Path::new("/home/someone/work"), cwd, Some(home)).unwrap_err();
        assert!(err.to_string().contains("delightfile is in"), "{err}");
        let err = check_deletable(cwd, cwd, Some(home)).unwrap_err();
        assert!(err.to_string().contains("delightfile is in"), "{err}");
        // A sibling of the cwd is fine.
        check_deletable(Path::new("/home/someone/work/other"), cwd, Some(home)).unwrap();
    }

    #[test]
    fn rail_is_not_fooled_by_dots_or_a_name_prefix() {
        let home = Path::new("/home/someone");
        let cwd = Path::new("/home/someone/work");
        let err =
            check_deletable(Path::new("/home/someone/work/.."), cwd, Some(home)).unwrap_err();
        assert!(err.to_string().contains("home directory"), "{err}");
        // `/home/someone/workshop` merely starts with the same letters.
        check_deletable(Path::new("/home/someone/workshop"), cwd, Some(home)).unwrap();
    }

    #[test]
    fn delete_permanent_applies_the_rails() {
        // `/` is above every possible cwd, so this fails whatever the
        // environment is.
        let err = delete_permanent(Path::new("/"), &ctx()).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[test]
    fn delete_permanent_removes_a_real_tree() {
        let t = TempTree::new("delete-perm");
        let victim = t.dir("victim/inner");
        std::fs::write(victim.join("f"), b"x").unwrap();
        delete_permanent(&t.join("victim"), &ctx()).unwrap();
        assert!(!exists(&t.join("victim")));
    }
}
