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
//! 1. a filesystem root: `/`, or on Windows a drive or share (`C:\`);
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

    if crate::path::is_root(&target) {
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
    // other rails, so an unreadable cwd falls back to the root the target
    // hangs from (`/`, or its drive), which the cwd rail then guards.
    let cwd = std::env::current_dir().unwrap_or_else(|_| crate::path::root_of(path));
    let home = crate::platform::dirs::home();
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
    // `lstat("link/")` answers about the *target*, so a walk that kept the
    // slash would descend a symlink and empty what it points at. Everything
    // that unlinks trims first.
    let trimmed = crate::path::trim_trailing_separator(path.as_os_str());
    let path = Path::new(&trimmed);
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        // Already gone is the state we wanted.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DfError::io(path, e)),
    };

    // A symlink to a directory is unlinked, never descended into: deleting a
    // link to ~/Pictures must not delete ~/Pictures.
    //
    // Every step below takes "already gone" as done, as the `lstat` above
    // does: the tree can be emptied by somebody else while this walks it — a
    // second delightfile purging the same trash, `rm -r` in a terminal — and a
    // directory that vanished between the `lstat` and the `read_dir`, or a
    // file between the `lstat` and the `unlink`, is the outcome that was asked
    // for, not a failure to report.
    if meta.is_dir() && !meta.is_symlink() {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if gone(&e) => return Ok(()),
            Err(e) => return Err(DfError::io(path, e)),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) if gone(&e) => continue,
                Err(e) => return Err(DfError::io(path, e)),
            };
            remove_tree(&entry.path(), ctx)?;
        }
        match std::fs::remove_dir(path) {
            Ok(()) => ctx.advance(0, 1),
            Err(e) if gone(&e) => {}
            Err(e) => return Err(DfError::io(path, e)),
        }
        return Ok(());
    }

    let len = if meta.is_symlink() { 0 } else { meta.len() };
    let removed = if meta.is_symlink() {
        // A link, of whichever kind: on Windows one to a directory is removed
        // as a directory, without following it.
        crate::platform::fs::remove_link(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => ctx.advance(len, 1),
        Err(e) if gone(&e) => {}
        Err(e) => return Err(DfError::io(path, e)),
    }
    Ok(())
}

/// Whether an io error says the thing is not there — which, to a delete, is
/// the thing having been deleted.
fn gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

/// Remove a tree with no cancellation and no rails.
///
/// Only for cleaning up something *this program just created* — a partial copy,
/// a destination the user chose to overwrite. Never reachable from a keystroke.
pub(crate) fn remove_tree_unchecked(path: &Path) -> Result<()> {
    let trimmed = crate::path::trim_trailing_separator(path.as_os_str());
    let path = Path::new(&trimmed);
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DfError::io(path, e)),
    };
    if meta.is_dir() && !meta.is_symlink() {
        std::fs::remove_dir_all(path).map_err(|e| DfError::io(path, e))
    } else if meta.is_symlink() {
        crate::platform::fs::remove_link(path).map_err(|e| DfError::io(path, e))
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
    use std::path::PathBuf;

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

    #[cfg(unix)]
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
    fn a_trailing_slash_does_not_turn_a_link_into_its_target() {
        // BUG: POSIX makes a trailing slash mean "and this is a directory", so
        // `lstat("link/")` follows the final symlink. The walk below therefore
        // saw a directory rather than a link, descended into it, and emptied
        // the thing the link pointed at — `~/link/` deleting `~/Pictures`.
        let t = TempTree::new("delete-trailing-slash");
        let real = t.dir("real");
        std::fs::write(real.join("precious"), b"keep").unwrap();
        let link = t.symlink(&real, "link");

        let with_slash = PathBuf::from(format!("{}/", link.display()));
        remove_tree(&with_slash, &ctx()).unwrap();
        assert!(real.join("precious").is_file(), "the target survives");
        assert!(!exists(&link), "the link itself is gone");
    }

    /// A link to a directory is removed as a link — on Windows, where such a
    /// link is a directory to the file system, with the call for one — and
    /// the directory it points at is untouched. Skipped where a link cannot
    /// be made (Windows without Developer Mode or elevation).
    #[test]
    fn removes_a_link_to_a_directory_and_nothing_it_points_at() {
        let t = TempTree::new("delete-dir-link");
        let real = t.dir("real");
        std::fs::write(real.join("precious"), b"keep").unwrap();
        let link = t.join("link");
        if let Err(e) = crate::platform::fs::symlink(&real, &link) {
            eprintln!("skipping: no link could be made here ({e})");
            return;
        }
        remove_tree(&link, &ctx()).unwrap();
        assert!(!exists(&link), "the link is gone");
        assert!(real.join("precious").is_file(), "the target survives");

        crate::platform::fs::symlink(&real, &link).unwrap();
        remove_tree_unchecked(&link).unwrap();
        assert!(!exists(&link));
        assert!(real.join("precious").is_file());
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

    /// Two deletes of one tree at once — two delightfiles purging the same
    /// trash — both succeed: whatever one finds already gone, the other took,
    /// and a directory that vanished between being seen and being opened, or
    /// being emptied and being removed, is not a failure.
    #[test]
    fn a_tree_emptied_by_somebody_else_meanwhile_is_not_a_failure() {
        for round in 0..20 {
            let t = TempTree::new(&format!("delete-race-{round}"));
            let victim = t.dir("victim");
            for a in 0..8 {
                for b in 0..8 {
                    let dir = victim.join(format!("{a}/{b}"));
                    std::fs::create_dir_all(&dir).unwrap();
                    for c in 0..4 {
                        std::fs::write(dir.join(format!("{c}.txt")), b"x").unwrap();
                    }
                }
            }
            let racers: Vec<_> = (0..2)
                .map(|_| {
                    let victim = victim.clone();
                    std::thread::spawn(move || remove_tree(&victim, &TaskCtx::detached()))
                })
                .collect();
            for racer in racers {
                let result = racer.join().expect("the delete panicked");
                assert!(result.is_ok(), "round {round}: {result:?}");
            }
            assert!(!exists(&victim));
        }
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

    /// Every root the platform has is one: a drive, a share, however spelled.
    #[test]
    fn rail_refuses_a_drive_and_a_share() {
        if !cfg!(windows) {
            return;
        }
        let cwd = Path::new(r"C:\Users\someone\work");
        let home = Some(Path::new(r"C:\Users\someone"));
        for root in [r"C:\", "C:/", r"D:\", r"\\server\share", r"\\server\share\"] {
            let err = check_deletable(Path::new(root), cwd, home).unwrap_err();
            assert!(err.to_string().contains("filesystem root"), "{root}: {err}");
        }
        check_deletable(Path::new(r"D:\junk"), cwd, home).unwrap();
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
        let err = check_deletable(Path::new("/home/someone/work/.."), cwd, Some(home)).unwrap_err();
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
