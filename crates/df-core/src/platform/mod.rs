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
//! | `errno` | `is_delete_pending(&io::Error) -> bool`: a name another deleter has marked and not yet let go, as good as gone (asked straight after the failing call, on its thread) | never (unix) | unix | access denied over `STATUS_DELETE_PENDING` (or a name not there), from `RtlGetLastNtStatus` (W4.37) |
//! | `watch` | `Backend::open(control, events, notify) -> io::Result<(Backend, JoinHandle<()>)>`, `Backend::wake(&self)` (crate-internal: [`crate::fs::Watcher`] is the API) | inotify + self-pipe | kqueue `EVFILT_VNODE` per directory + self-pipe | `ReadDirectoryChangesW`, one overlapped read per directory, a wake event; at most 63 directories (W4.4) |
//! | `fs` | `symlink(target, link) -> io::Result<()>` | `std::os::unix::fs::symlink` (unix) | unix | `symlink_dir` / `symlink_file` by what the target resolves to from the link's folder; without the privilege, `PermissionDenied` "Creating links needs Developer Mode or an elevated process" (W4.6) |
//! | `fs` | `apply_mode(path, mode: u32) -> io::Result<()>` | `chmod` (unix) | unix | read-only attribute from `mode & 0o222` |
//! | `fs` | `set_times(path, atime, mtime) -> Result<()>` (not through a link) | `utimensat`, `libc::UTIME_OMIT` (unix) | unix | `File::set_times` on the reparse point |
//! | `fs` | `sync_dir(dir) -> Result<()>` | `fsync`, `EINVAL` tolerated (unix) | unix | `Ok(())` |
//! | `fs` | `sync_file(path) -> io::Result<()>` (a file another program wrote, flushed) | `fsync` through a read-only handle (unix) | unix | `FlushFileBuffers` through a handle opened to write, which it needs (W4.32) |
//! | `fs` | `write_all_at(file, data, offset) -> io::Result<()>` | `FileExt::write_all_at` (unix) | unix | `seek_write` loop |
//! | `fs` | `same_file(a, b) -> io::Result<bool>` (not through a link) | `dev` + `ino` (unix) | unix | volume serial + file index, through `meta::identity` (P3.3) |
//! | `fs` | `writable(dir) -> bool` | `access(W_OK)` (unix) | unix | a hidden `.df-write-test-<pid>-<n>` made there, deleted on close (W4.6) |
//! | `fs` | `is_junction(path) -> bool` (the path itself, P3.7) | `false` (unix) | unix | reparse tag `IO_REPARSE_TAG_MOUNT_POINT` |
//! | `fs` | `is_emptying(dir) -> bool`: every name still listed is another deleter's, marked and not yet let go, so "not empty" will pass (W4.37) | `false` (unix) | unix | each name left refuses to open as delete-pending (or is gone) |
//! | `fs` | `remove_link(path) -> io::Result<()>` (the link, never its target, P3.26) | `remove_file` (unix) | unix | `remove_dir` for a link to a directory or a junction, else `remove_file` |
//! | `fs` | `reflink(reader, writer) -> bool` (clone into an open file) | `FICLONE` | `false` | `false` |
//! | `fs` | `clone_before_open(reader, dst) -> io::Result<bool>` (clone into a new file; `false`: copy the long way) | `Ok(false)` | `fclonefileat` (APFS) | `Ok(false)` |
//! | `fs` | `forget_cached(file, path)` | `posix_fadvise(DONTNEED)` | nothing (`F_NOCACHE` is no hint) | nothing |
//! | `fs` | `is_remote(path) -> bool`, `magic_of(path) -> Option<i64>` | `statfs` `f_type` against `du::REMOTE_FS_MAGIC` | `statfs` `f_fstypename` (`smbfs`, `nfs`, `macfuse`…) / `f_type` | `false` / `None` |
//! | `trash` | `TrashedItem { trash_root, name, original, deleted_at }` with `location()`, `files_path()`, `info_path()`, `is_orphan()` | freedesktop `files/` + `info/` | `trash_root` is the journal, `name` the one in the Trash folder; `location()` read from its journal line | `trash_root` the drive's `$Recycle.Bin`, `name` the one it had, `original`, `deleted_at`; `location()` empty — the bin's own name for it is not reported, so nothing here finds it again, and undo's check of `files_path()` fails until W4.8 answers it (W4.7) |
//! | `trash` | `Trash::{at, home, root, files_dir, trash, list, restore, purge}` | freedesktop spec | `NSFileManager.trashItemAtURL` and a journal line in the state directory; the root is the journal | `home` = `%SystemDrive%\$Recycle.Bin`; `trash` = `SHFileOperationW(FO_DELETE)` with `FOF_ALLOWUNDO`, `FOF_NOCONFIRMATION`, `FOF_SILENT`, `FOF_NOERRORUI`, and `FOF_WANTNUKEWARNING` so the shell asks before deleting a file too big for the bin for good, `Ok(TrashedItem)`, refused before the call for a missing path (`Io`, not found), the delete rails, a drive with no bin (`Op("<volume> has no Recycle Bin")`), 260 UTF-16 units or more (`Op("path too long for the Recycle Bin")`), a verbatim path with no plain spelling; a non-zero return is `Op` with the code, an abort (that question's No) `Op` "cancelled"; `list` `Ok(vec![])`; `restore`, `purge` `Unsupported("Restoring from the Recycle Bin")` (W4.7) |
//! | `trash` | `for_path(path) -> Result<Trash>`, `restore(item, ctx)`, `purge(item, ctx)` | spec §Trash directories | `home()` for every path; restore renames back, purge drops the line first | `for_path`: the bin of the volume the path is on when that is a fixed drive, else `Op("<volume> has no Recycle Bin")`; `restore`, `purge` as above |
//! | `trash` | `available_for(dest) -> bool`, `for_sync(dest) -> Result<Trash>` (a mirror's extras) | home or `$topdir` trash | a volume that is not remote / `home()` | `GetDriveTypeW` of the volume is `DRIVE_FIXED` (removable media and shares have no bin) / `for_path` |
//! | `trash` | `Purged`, `purge_expired`, `purge_expired_if_due`, `purge_due_in` | `[mgr] trash_keep_days` over the spec | over the journal's lines alone, by their UTC date; the stamp is `trash-journal.purge` beside it (M2.35) | `Unsupported("Emptying old items from the Recycle Bin")`; never due |
//! | `trash` | `iso8601_utc(SystemTime) -> String`, `parse_deletion_date(&str) -> Option<SystemTime>` | the `DeletionDate` text | the same text | the same text |
//! | `trash` | `BinSize { items, bytes }`; `bin_size() -> Result<BinSize>`, `empty_bin() -> Result<()>`: the whole bin, counted and emptied in one call ("Empty trash" where there is no view) | `Unsupported("The Recycle Bin")`: the view lists and purges | same | `SHQueryRecycleBinW` over every drive's bin; `SHEmptyRecycleBinW` with `SHERB_NOCONFIRMATION`, an empty bin `Ok` without the call; a failure `Op` with the code (W4.8) |
//! | `trash` | Linux only: `TRASHINFO_EXT`, `PURGE_STAMP`, `DATE_SLACK_SECS`, `home_trash_path`, `topdir_trash`, `mount_point_of`, `uid` (the `user` one, re-exported), `trashinfo_text`, `parse_trashinfo`, `encode_path`, `decode_path`, `expired`, `purge_wait`, `Trash::{info_dir, ensure}` | the spec's own vocabulary | — | — |
//! | `xattr` | `AVAILABLE: bool`; `get_raw(path, name) -> io::Result<Option<Vec<u8>>>`, `list_raw(path) -> io::Result<Vec<String>>`, `set_raw(path, name, value)`, `remove_raw(path, name)`; `quiet`, `is_absent`, `is_unsupported`, `is_not_permitted`: `fn(&io::Error) -> bool` | `l*xattr`, never through a link | `*xattr` with `XATTR_NOFOLLOW`; `user.xdg.tags` is Finder's `_kMDItemUserTags` plist; a link refused | nothing to read, writes `Unsupported("Tags")` |
//! | `nofollow` | `ready() -> Result<(), String>`; `Finder::{default, folder(anchor, names) -> Rc<Folder>, open(anchor, path, want_dir) -> (Entry, Stat, usize)}`; `open_in(dir, name, want_dir) -> Folder`, `stat_in(dir, name) -> Stat`, `read_dir_in(dir)` (entries with `file_name()`); `Entry::metadata() -> Stat`; `set_mode_of(entry, mode) -> Result<(), String>`; `swapped(&io::Error) -> bool`; `meta::{mode, dev, ino}` of a `Stat` (the permissions change's walk) | `O_PATH | O_NOFOLLOW` descriptors named through `/proc/self/fd`; `Folder`, `Entry` = `File`, `Stat` = `Metadata`, `meta` = `platform::meta` | `openat(O_NOFOLLOW)` from the folder above, `fstatat`/`fchmodat` with `AT_SYMLINK_NOFOLLOW` on the last name; a folder its owner shut is opened when next used | the same stub, with Linux's types, for good: permissions editing does not exist on Windows (04-windows.md Decisions log) |
//! | `meta` | `dev`, `ino`, `nlink` `-> u64`; `mode`, `uid`, `gid` `-> u32`; `change_time -> (i64, i64)`; `mtime -> i64`; each `fn(&Metadata)` | `MetadataExt` (unix) | unix | `0`, `0`, `1`; a mode made from the type and read-only flag; `0`, `0`; the last write |
//! | `meta` | `blocks_bytes(&Metadata, path: impl FnOnce() -> PathBuf) -> u64` (what it occupies on disk; `path` builds the path when the platform needs it) | `st_blocks` × 512, `path` never called (unix) | unix | a regular file's `GetCompressedFileSizeW`, else (or when it fails) the length (W4.35) |
//! | `meta` | `Identity { dev, ino, nlink }`; `identity(path, &Metadata) -> io::Result<Identity>` (the `symlink_metadata`: not through a link); `maybe_linked(&Metadata) -> bool` (P3.4) | off the `stat` (unix) | unix | `GetFileInformationByHandle` on a handle opened with `FILE_FLAG_OPEN_REPARSE_POINT`; any regular file may be linked |
//! | `meta` | `is_hidden(name: &OsStr, &Metadata) -> bool` (the row's own metadata) (P3.4) | a leading dot (unix) | unix: `UF_HIDDEN` not read | a leading dot or `FILE_ATTRIBUTE_HIDDEN` |
//! | `meta` | `is_executable(name: &OsStr, mode: u32) -> bool` (a row the name cannot classify, P3.28) | `mode & 0o111` (unix) | unix | the extension in `PATHEXT`, read once |
//! | `user` | `uid() -> u32` | `getuid` (unix) | unix | `0` |
//! | `user` | `cache_suffix() -> String` (the `yazi-<suffix>` thumbnail directory) | the uid (unix) | unix | `"0"`, yazi's `uid_or_zero` |
//! | `user` | `user_name(uid)`, `group_name(gid)` `-> Option<&'static str>` | `/etc/passwd`, `/etc/group`, read once | `getpwuid_r` / `getgrgid_r`, each id asked once | `None` |
//! | `thread` | `lower_priority(nice: i32) -> bool` (called with 1–19 by [`crate::thread::lower_priority`]) | `setpriority(PRIO_PROCESS, 0, nice)`: this thread | `pthread_set_qos_class_self_np(UTILITY)`: this thread | nothing, `false` |
//! | `time` | `local_civil(secs: i64) -> Option<rename::facts::Civil>` | `localtime_r` (unix) | unix | `FileTimeToSystemTime` + `SystemTimeToTzSpecificLocalTime`, 1601 onwards (W4.36) |
//! | `dirs` | `home`, `config_dir`, `state_dir`, `data_dir`, `cache_dir`, `runtime_dir`, `temp_dir`: `fn() -> Option<PathBuf>`, each the base a caller adds its own folder to | XDG, `$HOME` fallbacks (unix) | unix, but `runtime_dir` is `$TMPDIR` | `%USERPROFILE%`; `%APPDATA%`; `%LOCALAPPDATA%` for state, data and cache; `%TEMP%` for runtime and temp; an unset folder under the profile (D5.1) |
//! | `dirs` | `yazi_config_dir() -> Option<PathBuf>` (whose `vfs.toml` is read before ours) | `yazi` under `config_dir` (unix) | unix | `%APPDATA%\yazi\config` |
//! | `defaults` | `openers() -> Vec<(&str, String, bool, &str)>` (one command per row), `RULES: &[(&str, &str, &[&str])]`, `bookmarks() -> Vec<config::Bookmark>`: what [`crate::config::Config::default`] ships; `KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)]` (context, keys, command id, description) laid over the shipped keymap | `config::DEFAULT_*`, Brian's yazi config; no overrides | `open`, Terminal.app, Zed and mpv where installed; Linux's rules row for row with `open` where Linux has delightviewer; the `h c d w` bookmarks and `D` Desktop, `o` Documents; Cmd+C/W/V/Q and `input-copy` | §2.2's argv openers, each row's first program this machine has (on `PATH`, or in `App Paths` and run by its full path), a row with none not shipped (D5.4, W4.41); every file's Enter the system's own open, a font's the `install` verb, "Open with…" last; Home, the known folders, `C:\`, `%APPDATA%` as their real paths (W4.40); `g /` the Places card |
//! | `keys` | `LABELS: [(keymap::Modifier, &str); 4]` (each modifier's mark, in the order [`crate::keymap::Chord::label`] writes them), `SUPER_IS_CTRL: bool` (a `super+…` in `keymap.toml` is bound as `ctrl+…`, with a warning), `ctrl_name() -> &'static str` (the `ctrl` role in running text) | `Ctrl+ Alt+ Super+ Shift+`; `false`; "Ctrl" | `⌃ ⌥ ⇧ ⌘` run together, `ctrl` being `⌘`; `true`; "⌘" (M2.21) | as Linux |
//! | `os` | `as_bytes(&OsStr) -> Result<Cow<[u8]>>`, `from_bytes(&[u8]) -> Result<OsString>` (P3.1) | the bytes, never an error (unix) | unix | UTF-8; `Unsupported("non-Unicode file name")` otherwise |
//! | `os` | `STRICT_NAMES: bool`, `FOLD_CASE: bool` (what [`crate::path`] holds a name and a key to, P3.2) | `false`, `false` (unix) | unix | `true`, `true` |
//! | `os` | `MAX_NAME: usize`, `NAME_IN_UTF16: bool` (how long a made-up name may be, [`crate::fs::names`], P3.6) | 255 bytes (unix) | unix | 255 UTF-16 units |
//! | `os` | `HOME_AS_TILDE: bool` (a place under home is shown `~/…`; else as its whole path, W4.40) | `true` (unix) | unix | `false` |
//! | `pipe` | `Pipes::{open(stdin, stdout, stderr, read_size, stderr_cap, label) -> io::Result<Pipes>, read(&mut Vec<u8>, left) -> io::Result<Read>, write(&[u8], left) -> io::Result<Wrote>, drain_stderr(deadline), stderr() -> &[u8]}`; `Read::{Data, Eof, Nothing}`, `Wrote::{Bytes(n), Nothing, Broken}`; `remaining(deadline) -> Option<Duration>` (the SFTP child's pipes, waited on with deadlines) | `poll` before each read and write, stdin and stderr `O_NONBLOCK` (unix) | unix | a thread per pipe — `df-sftp-out`, `df-sftp-err` reading into bounded channels, `df-sftp-in` writing each buffer whole — waited on with `select` and `recv_timeout` (W4.20) |
//! | `socket` | `AVAILABLE: bool`; `PATH_MAX: usize` (the longest path `bind` takes); `Stream: Read + Write`; `connect(path, timeout) -> io::Result<Stream>`; `private_dir(dir, uid) -> io::Result<()>`; `dirs(uid) -> Vec<PathBuf>` (where the socket may go, the first private one that fits taken) (the rclone daemon's remote control); for tests, `Listener::{bind(path), accept() -> Stream}` | `UnixStream`; mode 0700 and owner check; 107; `$XDG_RUNTIME_DIR/delightfile`, then `delightfile-<uid>` under the temp dir and `/tmp` (unix) | unix; 103 | Windows' own `AF_UNIX` through WinSock, the path as UTF-8, a `select` with the timeout before each `recv` and `send`; 107; the directory made with the ACL it inherits, a link or junction refused; `%LOCALAPPDATA%\delightfile\run` alone (W4.32) |
//! | `process` | `NULL_DEVICE: &str` | `/dev/null` (unix) | unix | `NUL` |
//! | `process` | `HAS_RSYNC: bool` (then [`crate::sync::rsync::available`] asks for 3.1 or newer) | `true` (unix) | unix | `false` |
//! | `process` | `RSYNC_HINT: &str` (what "needs …" names in place of the bare "rsync"; empty: "rsync") | `""` | "rsync 3.1 or newer — `brew install rsync`" | `""` |
//! | `process` | `pause(&Child)`, `resume(&Child)` `-> io::Result<()>` | `SIGSTOP` / `SIGCONT` (unix) | unix | `Unsupported` (W4.2) |
//! | `process` | `terminate(&mut Child) -> io::Result<()>` (not a reaped one) | `kill(SIGTERM)` (unix) | unix | `Child::kill` |
//! | `process` | `TERMINATE_IS_GENTLE: bool` (whether the child `terminate` ends can clean up first; where not, the rclone daemon's drop stops its running job before it) | `true` (unix) | unix | `false` |
//! | `process` | `quiet(&mut Command) -> &mut Command`: a console tool run headless (`git`, `7z`, `tar`, the decompressors, `ssh`, `rclone`), never an opener | nothing (unix) | unix | `CREATE_NO_WINDOW`, the only creation flag df-core sets (W4.2) |
//! | `process` | `tie_to_this_thread(&mut Command)` before the start, `tie(&Child) -> io::Result<Tie>` after it, the `Tie` held for the child's life (re-exported as `vfs::child`) | `prctl(PR_SET_PDEATHSIG)` + `getppid` in `pre_exec`; `tie` nothing | `getppid`, then a watcher forked in `pre_exec` waits in kqueue on `NOTE_EXIT` and sends `SIGTERM`: tied to the process; `tie` nothing | nothing before; `tie` puts the child in a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, whose handle the `Tie` owns (W4.31) |
//! | `process` | `exit_code(&ExitStatus) -> i32` (what a shell's `$?` would say; df-app's blocking opener) | the code, else 128 + the signal (unix) | unix | the code, else 1 (never reached: Windows always has one) |
//! | `process` | `is_executable(&Path) -> bool` | an execute bit (unix) | unix | extension in `PATHEXT` |
//! | `process` | `candidates(name) -> Vec<String>` (file names to look for on `PATH`) | `[name]` (unix) | unix | `[name.exe, name]`, and `tar.exe`, `tar` for `bsdtar`, `7za.exe`, `7za` for `7z` (D5.7) |

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
