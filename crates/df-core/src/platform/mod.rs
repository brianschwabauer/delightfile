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
//! | `fs` | `same_file(a, b) -> io::Result<bool>` (not through a link) | `dev` + `ino` (unix) | unix | volume serial + file index, through `meta::identity` (P3.3) |
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
//! | `trash` | Linux only: `TRASHINFO_EXT`, `PURGE_STAMP`, `DATE_SLACK_SECS`, `home_trash_path`, `topdir_trash`, `mount_point_of`, `uid` (the `user` one, re-exported), `trashinfo_text`, `parse_trashinfo`, `encode_path`, `decode_path`, `expired`, `purge_wait`, `Trash::{info_dir, ensure}` | the spec's own vocabulary | — | — |
//! | `xattr` | `AVAILABLE: bool`; `get_raw(path, name) -> io::Result<Option<Vec<u8>>>`, `list_raw(path) -> io::Result<Vec<String>>`, `set_raw(path, name, value)`, `remove_raw(path, name)`; `quiet`, `is_absent`, `is_unsupported`, `is_not_permitted`: `fn(&io::Error) -> bool` | `l*xattr`, never through a link | nothing to read, writes `Unsupported("Tags")` (open question in 02-macos.md) | same stub |
//! | `nofollow` | `ready() -> Result<(), String>`; `Finder::{default, folder(anchor, names), open(anchor, path, want_dir)}`; `open_in(dir, name, want_dir)`, `stat_in(dir, name)`, `read_dir_in(dir)`; `set_mode_of(file, mode) -> Result<(), String>`; `swapped(&io::Error) -> bool` (the permissions change's walk) | `O_PATH | O_NOFOLLOW` descriptors named through `/proc/self/fd` | `ready()` refuses: `Unsupported("Permissions")` (M2.28) | same stub (open question in 04-windows.md) |
//! | `meta` | `dev`, `ino`, `nlink`, `blocks_bytes` `-> u64`; `mode`, `uid`, `gid` `-> u32`; `change_time -> (i64, i64)`; `mtime -> i64`; each `fn(&Metadata)` | `MetadataExt` (unix) | unix | `0`, `0`, `1`, the size (W4.35); a mode made from the type and read-only flag; `0`, `0`; the last write |
//! | `meta` | `Identity { dev, ino, nlink }`; `identity(path, &Metadata) -> io::Result<Identity>` (the `symlink_metadata`: not through a link); `maybe_linked(&Metadata) -> bool` (P3.4) | off the `stat` (unix) | unix | `GetFileInformationByHandle` on a handle opened with `FILE_FLAG_OPEN_REPARSE_POINT`; any regular file may be linked |
//! | `meta` | `is_hidden(name: &OsStr, &Metadata) -> bool` (the row's own metadata) (P3.4) | a leading dot (unix) | unix: `UF_HIDDEN` not read | a leading dot or `FILE_ATTRIBUTE_HIDDEN` |
//! | `user` | `uid() -> u32` | `getuid` (unix) | unix | `0` |
//! | `user` | `cache_suffix() -> String` (the `yazi-<suffix>` thumbnail directory) | the uid (unix) | unix | `"0"`, yazi's `uid_or_zero` |
//! | `user` | `user_name(uid)`, `group_name(gid)` `-> Option<&'static str>` | `/etc/passwd`, `/etc/group`, read once | `None` (M2.4) | `None` |
//! | `thread` | `lower_priority(nice: i32) -> bool` (called with 1–19 by [`crate::thread::lower_priority`]) | `setpriority(PRIO_PROCESS, 0, nice)`: this thread | nothing, `false` (M2.5) | nothing, `false` |
//! | `time` | `local_civil(secs: i64) -> Option<rename::facts::Civil>` | `localtime_r` (unix) | unix | `localtime_s` |
//! | `dirs` | `home`, `config_dir`, `state_dir`, `data_dir`, `cache_dir`, `runtime_dir`, `temp_dir`: `fn() -> Option<PathBuf>` | XDG, `$HOME` fallbacks (unix) | unix, the same (D5.1) | `%USERPROFILE%` and `%TEMP%`; the rest `None` (D5.1) |
//! | `os` | `as_bytes(&OsStr) -> Result<Cow<[u8]>>`, `from_bytes(&[u8]) -> Result<OsString>` (P3.1) | the bytes, never an error (unix) | unix | UTF-8; `Unsupported("non-Unicode file name")` otherwise |
//! | `os` | `STRICT_NAMES: bool`, `FOLD_CASE: bool` (what [`crate::path`] holds a name and a key to, P3.2) | `false`, `false` (unix) | unix | `true`, `true` |
//! | `os` | `MAX_NAME: usize`, `NAME_IN_UTF16: bool` (how long a made-up name may be, [`crate::fs::names`], P3.6) | 255 bytes (unix) | unix | 255 UTF-16 units |
//! | `pipe` | `AVAILABLE: bool`; `fd(&pipe) -> i32`; `remaining(deadline)`; `poll_read2(stdout, stderr, timeout) -> io::Result<(bool, bool)>`; `poll_write(fd, timeout) -> io::Result<bool>`; `set_nonblocking(fd) -> io::Result<()>` | `poll`, `fcntl` (unix) | unix | `AVAILABLE = false`, so SFTP refuses before spawning; the rest `Unsupported("SFTP")` (W4.20) |
//! | `socket` | `AVAILABLE: bool`; `Stream: Read + Write`; `connect(path, timeout) -> io::Result<Stream>`; `private_dir(dir, uid) -> io::Result<()>` (the rclone daemon's remote control) | `UnixStream`; mode 0700 and owner check (unix) | unix | `AVAILABLE = false`, so cloud remotes refuse before `rclone` starts; the rest `Unsupported("Cloud remotes")` (W4.32) |
//! | `process` | `NULL_DEVICE: &str` | `/dev/null` (unix) | unix | `NUL` |
//! | `process` | `HAS_RSYNC: bool` | `true` (unix) | unix; the version gate is M2.6 | `false` |
//! | `process` | `pause(&Child)`, `resume(&Child)` `-> io::Result<()>` | `SIGSTOP` / `SIGCONT` (unix) | unix | `Unsupported` (W4.2) |
//! | `process` | `terminate(&mut Child) -> io::Result<()>` (not a reaped one) | `kill(SIGTERM)` (unix) | unix | `Child::kill` |
//! | `process` | `tie_to_this_thread(&mut Command)` (re-exported as `vfs::child`) | `prctl(PR_SET_PDEATHSIG)` + `getppid` in `pre_exec` | nothing: the child can outlive a crash (M2.29) | nothing (W4.31) |
//! | `process` | `exit_code(&ExitStatus) -> i32` (what a shell's `$?` would say; df-app's blocking opener) | the code, else 128 + the signal (unix) | unix | the code, else 1 (never reached: Windows always has one) |
//! | `process` | `is_executable(&Path) -> bool` | an execute bit (unix) | unix | extension in `PATHEXT` |
//! | `process` | `candidates(name) -> Vec<String>` (file names to look for on `PATH`) | `[name]` (unix) | unix | `[name.exe, name]`, and `tar.exe`, `tar` for `bsdtar` |

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
