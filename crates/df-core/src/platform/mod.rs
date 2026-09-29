//! The platform seam: everything in df-core whose body depends on the operating
//! system, behind one module that offers the same set of functions on every
//! target.
//!
//! delightfile was written for Linux, and most of it is plain Rust that does
//! not care. The parts that do care — a syscall `std` does not expose, a
//! Unix-only extension trait, a freedesktop convention — live under this module
//! and nowhere else. Code outside it is written once and calls
//! `platform::<module>::<function>`; it never sees a `cfg`.
//!
//! Inside, each target has a module of its own (`linux`, `macos`, `windows`)
//! that provides exactly the set in the table below; `unix` holds the bodies
//! Linux and macOS share. Selection is `#[cfg]` on the `mod` lines and a glob
//! `pub use`, with no traits and no `dyn`: three implementations and one caller
//! do not need them, and the CI matrix compiling all three targets is what keeps
//! the sets in step (`plans/other-platforms/00-ground-rules.md` §2).
//!
//! A target that has no implementation of a feature yet gets an honest stub:
//! an operation fails with [`crate::DfError::Unsupported`], a query answers
//! "nothing", and a service comes up inert. A stub never panics and never
//! claims to have done work it did not do.
//!
//! # The contract
//!
//! Every function and constant the rest of the crate (and df-app) may use, and
//! what each target does for it. A signature changes here in the same change
//! that changes it in the target modules.
//!
//! | Module | Item | Linux | macOS | Windows |
//! |---|---|---|---|---|
//! | `errno` | `is_cross_device`, `is_exists`, `is_not_empty`, `is_not_dir`, `is_dir`, `is_invalid`, `is_transient`: `fn(&io::Error) -> bool` | `libc::E*` (unix) | `libc::E*` (unix) | Win32 codes; `is_dir` never |
//! | `watch` | `Backend::open(control, events, notify) -> io::Result<(Backend, JoinHandle<()>)>`, `Backend::wake(&self)` (crate-internal: [`crate::fs::Watcher`] is the API) | inotify + self-pipe | `Unsupported` → disabled watcher (M2.1) | `Unsupported` → disabled watcher (W4.4) |
//! | `fs` | `symlink(target, link) -> io::Result<()>` | `std::os::unix::fs::symlink` (unix) | unix | `symlink_dir` / `symlink_file` by what the target resolves to |
//! | `fs` | `apply_mode(path, mode: u32) -> io::Result<()>` | `chmod` (unix) | unix | read-only attribute from `mode & 0o222` |
//! | `fs` | `set_times(path, atime, mtime) -> Result<()>` (not through a link) | `utimensat`, `libc::UTIME_OMIT` (unix) | unix | `File::set_times` on the reparse point |
//! | `fs` | `sync_dir(dir) -> Result<()>` | `fsync`, `EINVAL` tolerated (unix) | unix | `Ok(())` |
//! | `fs` | `write_all_at(file, data, offset) -> io::Result<()>` | `FileExt::write_all_at` (unix) | unix | `seek_write` loop |
//! | `fs` | `same_file(a, b) -> io::Result<bool>` (not through a link) | `dev` + `ino` (unix) | unix | `Unsupported` (P3.3) |
//! | `fs` | `writable(dir) -> bool` | `access(W_OK)` (unix) | unix | `true` (W4.6) |
//! | `fs` | `reflink(reader, writer) -> bool` | `FICLONE` | `false` (M2.2) | `false` |
//! | `fs` | `forget_cached(file, path)` | `posix_fadvise(DONTNEED)` | nothing (M2.7) | nothing |
//! | `fs` | `is_remote(path) -> bool`, `magic_of(path) -> Option<i64>` | `statfs` `f_type` against `du::REMOTE_FS_MAGIC` | `false` / `None` (M2.3) | `false` / `None` |
//! | `trash` | `TrashedItem { trash_root, name, original, deleted_at }` with `location()`, `files_path()`, `info_path()`, `is_orphan()` | freedesktop `files/` + `info/` | empty paths, never an orphan (M2.8) | same stub (W4.7) |
//! | `trash` | `Trash::{at, home, root, files_dir, trash, list, restore, purge}` | freedesktop spec | `home` and every operation `Unsupported("Trash")`, `list` empty | same stub |
//! | `trash` | `for_path(path) -> Result<Trash>`, `restore(item, ctx)`, `purge(item, ctx)` | spec §Trash directories | `Unsupported("Trash")` | same stub |
//! | `trash` | `available_for(dest) -> bool`, `for_sync(dest) -> Result<Trash>` (a mirror's extras) | home or `$topdir` trash | `false` / `Unsupported("Trash")` | same stub |
//! | `trash` | `Purged`, `purge_expired`, `purge_expired_if_due`, `purge_due_in` | `[mgr] trash_keep_days` over the spec | `Unsupported("Trash")`; `purge_due_in` is never due | same stub |
//! | `trash` | `iso8601_utc(SystemTime) -> String`, `parse_deletion_date(&str) -> Option<SystemTime>` | the `DeletionDate` text | the same text | the same text |
//! | `trash` | Linux only: `TRASHINFO_EXT`, `PURGE_STAMP`, `DATE_SLACK_SECS`, `home_trash_path`, `topdir_trash`, `mount_point_of`, `uid`, `trashinfo_text`, `parse_trashinfo`, `encode_path`, `decode_path`, `expired`, `purge_wait`, `Trash::{info_dir, ensure}` | the spec's own vocabulary | — | — |

#[cfg(unix)]
mod unix;

#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
