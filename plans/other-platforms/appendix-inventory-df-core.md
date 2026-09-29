# df-core platform inventory

`crates/df-core` is delightfile's headless library: the directory model and watcher, file operations with the undo journal and a hand-rolled freedesktop trash, recursive sizes (`du`), git status, archive listing/extraction/creation, local and rsync-backed sync, an SFTP client over a spawned `ssh`, config/state/zoxide readers, preview decisions and bulk rename. Its dependencies are `thiserror`, `log`, `crossbeam-channel`, `libc` (0.2.189 per `Cargo.lock`), `miniz_oxide` and `regex`. `src/` holds 94 `.rs` files and 61,021 lines: 38,159 outside test code, and 12,361 in dedicated `tests.rs` files (the rest is inline `mod tests` blocks). No `cfg(unix)`, `cfg(windows)` or `cfg(target_os)` gate appears anywhere in the crate, so every site below is compiled on every target. §1 lists **188 sites in 41 files**, §3 lists **107 path-model rows** in 7 tables, §4 lists **14 spawn sites**, and §5 sizes **36 test files** that contain at least one test with a Unix assumption. Method: grep for candidates, then read each site. Per-target symbol availability comes from the vendored `libc-0.2.189` source (`src/unix/bsd/apple/mod.rs`, `src/windows/mod.rs`). No cross-compile was run, because this machine has no macOS or Windows standard library installed.

**Class column legend**

- **Linux-only**: does not compile on macOS, or compiles there and gives a wrong result or no result. Windows is affected too.
- **Unix-only**: compiles and works on macOS, but does not compile on Windows. The cause is `std::os::unix::*`, or a `libc` item that the `windows` target of libc 0.2.189 does not define. That target does not define `getuid`, `poll`/`pollfd`, `fcntl`, `ioctl`, `kill`, `pid_t`, `SIGSTOP`/`SIGCONT`, `setpriority`, `statfs`, `utimensat`, `AT_*`, `W_OK`, `localtime_r` (it has `localtime_s`) or `posix_fadvise`. It does define the CRT errno constants (`EXDEV`=18, `EEXIST`=17, `EINVAL`=22, `ENOTEMPTY`=41, …), `access`, `read`, `write`, `close`, `timespec` and `tm`.
- **Windows-differs**: compiles on Windows, or would once the Unix-only lines around it are replaced, but behaves differently there.
- **macOS-differs**: compiles on macOS but behaves differently than on Linux, in a way milder than a wrong result.
- **(case)**: a comparison that is case-sensitive by construction. NTFS and default APFS volumes are case-insensitive, so these differ on both Windows and macOS.

"Used by" names callers outside the file as `core:<path>:<fn>` (df-core) or `app:<path>:<fn>` (df-app/src). Callers inside test modules are omitted.

---

## 1. Sites by module

### archive/external.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 63 | `std::env::var_os("PATH")` into `extractor_on` in `external_extractor()` | Windows-differs | app:app.rs:extract_paths; app:preview/listing.rs:seven_zip | `split_paths` already uses `;` on Windows |
| 75–78 | `extractor_on`: `split_paths(path).map(\|dir\| dir.join(name))` for `"7z"`, then `"bsdtar"` | Windows-differs | as above | Candidate names carry no `.exe`. Windows 10+ ships libarchive's bsdtar as `tar.exe`, not `bsdtar` ✓ S1.13 |
| 82–85 | `is_executable`: `std::os::unix::fs::PermissionsExt`, `mode() & 0o111 != 0` | Unix-only | `extractor_on`; core:archive/write/mod.rs:on_path | Windows has no execute bits ✓ S1.13 |
| 207–217 | `run_in`: `Command::new(program)…spawn()` | Windows-differs | core:archive/whole.rs:by_extractor, two_stage; core:archive/write/mod.rs:seven_zip | See §4 |

### archive/extract.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 212–216 | `destination_for`: `dest_dir.join(&entry.path)` once `name_is_unsafe` passes | Windows-differs | `plan_extract` ← app:app.rs:spawn_extract; core:archive/whole.rs:read_here | Names with `:` `<` `>` `"` `\|` `?` `*`, control characters, a trailing `.` or space, or a reserved device name (`CON`, `NUL`, `COM1`…) are not refused and are joined verbatim |

### archive/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 131–138 | `ArchiveFormat::decompressor()` returns `"gzip"`, `"xz"`, `"zstd"` | Windows-differs | `list_compressed_tar`; core:archive/unpack.rs:extract_compressed_tar; `is_available` | Looked up by bare name on `PATH`. None of the three is in a stock Windows install |
| 329–344 | `list_compressed_tar`: `Command::new(binary).arg("-dc")` with the archive `File` as stdin | Windows-differs | `list_until`/`list` ← app:app.rs:ask_archive; app:preview/listing.rs:list_with | §4 |
| 392–401 | `have(binary)`: `binary --version`, `.status()` | Windows-differs | `ArchiveFormat::is_available`; core:archive/write/mod.rs:Format::is_available ← app:app/compress.rs:open_archive_prompt | §4 |

### archive/tree.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 469–494 | `name_is_unsafe`: refuses a leading `/` or `\`, an `X:` drive prefix, `..`, `.git` in any ASCII case, NUL, and more than `MAX_NAME_BYTES` (4096) bytes | Windows-differs | `tree::build` (sets `unsafe_name`); core:archive/extract.rs:destination_for | No rule for Windows-reserved names or characters (see extract.rs:212) |

### archive/unpack.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 600–615 | `extract_compressed_tar`: `std::process::Command::new(binary).arg("-dc")` | Windows-differs | `extract` ← core:ops/jobs.rs:ExtractJob::run (← app:app.rs:spawn_extract); core:archive/whole.rs:read_here | §4 |

### archive/write/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 98–105 | `Format::tool()`: `"zstd"`, `"xz"`, `"7z"` | Windows-differs | `is_available`, `piped`, `seven_zip` | Bare names |
| 116–121 | `Format::is_available`: `on_path("7z")` or `super::have(tool)` | Windows-differs | app:app/compress.rs:open_archive_prompt | |
| 187, 213 | `extension_of` / `named`: `text.rsplit('/')` takes the leaf of a typed archive name | Windows-differs | `extension` ← app:app/compress.rs:format_hint; `named` ← app:app/compress.rs:archive_submit | A typed `out\photos.zip` is not split on `\` ✓ P3.16 |
| 529–536 | `walk`: `use std::os::unix::ffi::OsStrExt`; member name `name.as_bytes().to_vec()` | Unix-only | `Pack::run` ← app:app/compress.rs:spawn_archive | ✓ S1.16 ✓ P3.7 |
| 542–543 | `visit`: `use std::os::unix::ffi::OsStrExt; use std::os::unix::fs::MetadataExt` | Unix-only | as above | ✓ S1.16 ✓ P3.7 |
| 553–556 | `Member { mode: meta.mode(), mtime: meta.mtime(), uid: meta.uid(), gid: meta.gid() }` | Unix-only | Consumed by write/tar.rs:175–181 and write/zip.rs:218–235 | ✓ S1.7 ✓ P3.7 |
| 561–562 | Symlink target `read_link(path)…as_os_str().as_bytes()` | Unix-only | as above | Link text is stored as raw bytes ✓ S1.16 ✓ P3.7 |
| 566, 579–580 | Member names built as bytes: `push(b'/')`, `extend_from_slice(child.as_bytes())` | Unix-only | as above | `/` is the archive format's own separator. The child bytes come from `OsStr` ✓ S1.16 ✓ P3.7 |
| 656–661 | `on_path`: `var_os("PATH")`, `dir.join(name)`, `external::is_executable` | Windows-differs (+ Unix-only via `is_executable`) | `Format::is_available`, `seven_zip` | No `.exe` suffix ✓ S1.13 |
| 684–696 | `piped`: `Command::new(tool)` with `-q -c -T0` or `-z -c -q -T0` | Windows-differs | `Pack::run` | §4 |
| 762–782 | `seven_zip`: `7z a -t7z -bd -y -snl -- <temp> <names…>` via `external::run_in(…, Some(dir), …)` | Windows-differs | `Pack::run` | §4 |

### archive/write/tar.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 175–181 | `header`: `mode & 0o7777`, `member.uid`, `member.gid`, `member.mtime` into ustar fields | Windows-differs | `tar::write` ← write/mod.rs `Pack::run`, `piped` | The values come from Unix `MetadataExt` at write/mod.rs:553–556 |

### archive/write/zip.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 95 | `MADE_BY: u16 = (3 << 8) \| 63` (host = Unix) | Windows-differs | `local`/central header writers (418, 491) | External attributes carry `st_mode` |
| 218–235 | `external = ((S_IFDIR\|S_IFLNK\|S_IFREG) \| (member.mode & 0o7777)) << 16` | Windows-differs | `add` ← `Pack::run` | Mode comes from Unix `MetadataExt` |

### config.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 155–169 | `DEFAULT_BOOKMARKS`: `~`, `~/.config`, `~/Downloads`, `~/Work`, `/mnt/schwabserverroot…`, `sftp://showandtour{1,2}` | Windows-differs (paths also absent on macOS) | `default_bookmarks` ← core:keymap/defaults.rs (452), `Config::default` | |
| 192–282 | `DEFAULT_OPENERS`: POSIX-shell command strings: `setsid uwsm-app -- …`, `"${TERMINAL:-ghostty}"`, `"${EDITOR:-vi}"`, `zeditor`, `google-chrome-stable`, `delightviewer`, `pinta`, `system-cmd-wallpaper-set`, `system-cmd-image-optimize-yazi`, `xdg-open "$1"`, `mpv`, `$(dirname "$1")`, `>/dev/null 2>&1` | Linux-only | `Config::default` → `opener()`/`openers_for()` ← app:open.rs:choices_for, app:open.rs:from (copies `opener.command`) | Spawned by df-app, not df-core. `setsid` (util-linux), `uwsm-app` and `xdg-open` are Linux |
| 306–375 | `DEFAULT_RULES`: glob/mime rules naming those openers (`open` = `xdg-open`) | Linux-only (via openers) | `Config::openers_for` ← app:open.rs:choices_for | |
| 767–775 | `expand_home`: `var_os("HOME")`, `format!("{}{}", home.to_string_lossy(), rest)` | Windows-differs | `Bookmark::expanded_path` ← app:app.rs:goto, app:app/places.rs:pool, app:finder.rs:merge_places; core:state/pins.rs:expanded_path, same_place; app:app/places.rs:shown | `HOME` is not set by default on Windows (`USERPROFILE` is). The string splice keeps the `/` from `~/Work` ✓ S1.17 ✓ P3.14 |
| 1220–1229 | `Theme::dir_icon`: a pattern containing `/` is matched against the full path string | Windows-differs | app:icons.rs:icon_for (passes `entry.path.to_string_lossy()`) | Windows path strings use `\`, so such a pattern never matches ✓ P3.15 |
| 1400–1407 | `config_dir()`: `$XDG_CONFIG_HOME` (non-empty) or `$HOME/.config`, then `/delightfile` | Windows-differs; macOS-differs (not `~/Library/…`) | `load` ← app:app.rs:new; app:app.rs:new; core:vfs/config.rs:config_paths | Returns `None` on Windows unless `HOME` is set ✓ S1.11 |

### du/fstype.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 33–46 | `REMOTE_FS_MAGIC`: Linux `f_type` magics (NFS 0x6969, CIFS, SMB2, FUSE 0x65735546, …) | Linux-only (macOS: compiles, never matches) | `is_remote` | In the Apple `libc::statfs` layout, `f_type` is a `u32` Darwin type number, with the name in `f_fstypename: [c_char; 16]` ✓ S1.5 |
| 61–80 | `magic_of`: `OsStrExt::as_bytes` → `CString`, `libc::statfs`, `buf.f_type as i64` | Linux-only (macOS: compiles; Windows: no compile) | `is_remote` ← app:app.rs:poll_folders, begin_folder_sizes | ✓ S1.5 |

### du/walk.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 60 | `use std::os::unix::fs::MetadataExt` | Unix-only | file-wide | ✓ S1.7 |
| 364, 384 | `child_counts`: `root_meta.dev()`, `meta.dev()` → `crosses_boundary` | Unix-only | core:du/scanner.rs:run_walk | Windows has no `st_dev` ✓ S1.7 |
| 425–427 | `pub fn crosses_boundary(root_dev: u64, child_dev: u64, cross_filesystems: bool)` | Windows-differs | `walk_reusing`, `child_counts` | API is keyed on a Unix device number |
| 430–432 | `sizes_of`: `meta.blocks().saturating_mul(512)`, `meta.size()` | Unix-only | `walk_reusing` | ✓ S1.7 |
| 525, 616 | `walk_reusing`: `root_meta.dev()`, `meta.dev()` | Unix-only | `walk` ← core:du/scanner.rs:run_walk; `walk_blocking` ← `du_blocking` | ✓ S1.7 |
| 682–696 | `meta.nlink() > 1` → `seen_links.insert((meta.dev(), meta.ino()))` | Unix-only | `walk_reusing` | ✓ S1.7 |

### fs/entry.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 119 | `from_parts`: `use std::os::unix::fs::MetadataExt` | Unix-only | core:fs/scan.rs:run_scan, scan_blocking; `Entry::read` ← app:app.rs:archive_units, open_local_temp; app:dialog.rs:read; app:search.rs:parse; core:preview/job.rs:build | ✓ S1.7 |
| 121 | `is_hidden = name.starts_with('.')` | Windows-differs; macOS-differs | core:fs/filter.rs:filter_indices; app:ui.rs:listing; app:spot.rs:from_entry; app:trashview.rs:row_from; app:archive.rs:row | Windows marks hidden files with an attribute bit; macOS also has the `UF_HIDDEN` flag ✓ P3.4 |
| 164 | `classify(kind, &name, mime, meta.mode())` | Unix-only | as line 119 | ✓ S1.7 |
| 173–175 | `mode: meta.mode(), uid: meta.uid(), gid: meta.gid()` | Unix-only | Field readers: app:spot.rs:from_entry, permissions; app:remote.rs:card_rows; app:format.rs:linemode_text; app:app.rs:set_mode (`chmod` via `PermissionsExt` in df-app) | ✓ S1.7 |
| 236–270 | `permissions_string`: decodes `st_mode` type bits (0o170000) and rwx/setuid/sticky | Windows-differs | app:format.rs:linemode_text; app:remote.rs:card_rows | |
| 273–275 | `owner_label` → `fs::owner::owner_label(uid, gid)` | see fs/owner.rs | app:format.rs:linemode_text; app:remote.rs:card_rows | |

### fs/inotify.rs (the whole file; `mod inotify;` at fs/mod.rs:51 is private, and its only user is fs/watch.rs)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 28 | `use std::os::unix::ffi::OsStrExt` | Unix-only | `add_watch` | ✓ S1.4 |
| 46–54 | `pub const WATCH_MASK` from `libc::IN_CREATE \| … \| IN_ONLYDIR` | Linux-only (macOS: no compile) | fs/watch.rs:set_watches | libc apple defines no `IN_*` ✓ S1.4 |
| 71–78 | `Event::is_overflow` (`IN_Q_OVERFLOW`), `is_self_gone` (`IN_DELETE_SELF\|IN_MOVE_SELF\|IN_IGNORED`) | Linux-only | fs/watch.rs:run | ✓ S1.4 |
| 92–99 | `Inotify::new`: `libc::inotify_init1(IN_NONBLOCK \| IN_CLOEXEC)` | Linux-only | fs/watch.rs:Watcher::new | ✓ S1.4 |
| 108–118 | `add_watch`: `CString::new(path.as_os_str().as_bytes())`, `libc::inotify_add_watch` | Linux-only | fs/watch.rs:set_watches | ✓ S1.4 |
| 120–125 | `rm_watch`: `libc::inotify_rm_watch` | Linux-only | fs/watch.rs:set_watches | ✓ S1.4 |
| 129–150 | `read_events`: `libc::read` on the inotify fd into `[u64; 512]` | Linux-only | fs/watch.rs:run | ✓ S1.4 |
| 153–159 | `Drop for Inotify`: `libc::close` | Unix-only | | ✓ S1.4 |
| 163–196 | `HEADER = size_of::<libc::inotify_event>()`, `parse_events` with `read_unaligned::<libc::inotify_event>` | Linux-only | `read_events` | ✓ S1.4 |
| 210–222 | `Pipe::new`: `libc::pipe2(fds, O_CLOEXEC \| O_NONBLOCK)` | Linux-only (macOS: no compile, libc apple has no `pipe2`) | fs/watch.rs:Watcher::new | ✓ S1.4 |
| 231–241 | `Pipe::wake`: `libc::write` | Unix-only | fs/watch.rs:Watcher::interrupt | ✓ S1.4 |
| 244–259 | `Pipe::drain`: `libc::read` loop | Unix-only | fs/watch.rs:run | ✓ S1.4 |
| 262–271 | `Drop for Pipe`: `libc::close` ×2 | Unix-only | | ✓ S1.4 |
| 280–312 | `poll_two`: `libc::pollfd`, `libc::POLLIN`, `libc::poll` on two fds | Unix-only (Windows: no `poll`) | fs/watch.rs:run | ✓ S1.4 |

### fs/kind.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 315–344 | `pub fn classify(kind, name, mime, mode: u32)` → `is_special_mode(mode)` | Windows-differs | core:fs/entry.rs:from_parts; core:vfs/mod.rs:remote_entry; app:trashview.rs:row_from | A mode of 0 is treated as "unknown" and falls through to name rules |
| 351–357 | `const S_IFMT = 0o170_000`, `S_IFREG = 0o100_000`; `is_special_mode` | Windows-differs | `classify`, `kind_for_name` | Unix `st_mode` type encoding |
| 402 | `kind_for_name`: `mode & 0o111 != 0` → `FileKind::Executable` | Windows-differs | `classify`; `pub fn kind_for_name` | Windows has no execute bit |

### fs/owner.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 25–28 | `users()`: `parse_id_file("/etc/passwd")` | Linux-only (macOS: compiles, incomplete) | `user_name`, `owner_label` ← core:fs/entry.rs:owner_label ← app:format.rs:linemode_text, app:remote.rs:card_rows; app:spot.rs:rows | macOS `/etc/passwd` lists system accounts only (users come from Open Directory). Windows: the file is absent, so owners print as numbers ✓ S1.17 |
| 31–34 | `groups()`: `parse_id_file("/etc/group")` | Linux-only (as above) | `group_name`, `owner_label` | ✓ S1.17 |
| 39–84 | uid/gid `u32` → name model (`user_name(uid: u32)`, `group_name(gid: u32)`, `owner_label(uid, gid)`) | Windows-differs | as above | Windows owners are SIDs ✓ S1.17 |

### fs/tags.rs (added 2026-09-29: file tags postdate the inventory; lines as of 6aee8d1)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 52 | `use std::os::unix::ffi::OsStrExt` | Unix-only | `c_path`, `Find::next` | ✓ S1.16, S1.18 |
| 141, 154–155, 290, 412–430, 451 | errno: `ENODATA`, `ENOTSUP`, `EPERM`, `ENOSYS`, `ERANGE` read off attribute calls | Linux-only (macOS numbers differ; `ENODATA` is `ENOATTR` there) | `write`, `refusal`, `quiet`, `sized`, `get_raw`, test `set_raw` | ✓ S1.18 |
| 359 | `Find::next`: `file_name().as_bytes().first() == Some(&b'.')` for the hidden check | Unix-only | `find` ← app:search.rs | ✓ S1.16 |
| 474–519 | `lgetxattr`, `llistxattr`, `lsetxattr`, `lremovexattr` | Linux-only (macOS: no compile; its calls take an options word) | `get_raw`, `list_raw`, `set_raw`, `remove_raw` ← `read`, `write`, `find`, `carry` (← core:ops/copy.rs, core:sync/execute.rs) | ✓ S1.18 |

### fs/watch.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 41 | `use super::inotify::{self, Inotify, Pipe, WATCH_MASK}` | Linux-only | | ✓ S1.4 |
| 94–111 | `Watcher::new`: `Inotify::new()?`, `Pipe::new()?`, spawns thread `df-watch` running `run` | Linux-only | `Watcher::start` ← app:app.rs:assemble | On error, `start` falls back to `Watcher::disabled()` (122: log text names inotify) ✓ S1.4 |
| 147–167 | `Watcher::watch` / `interrupt` → `Pipe::wake` | Linux-only (via `Pipe`) | app:app.rs:assemble, rewatch; `drain` ← app:app.rs:poll_workers | ✓ S1.4 |
| 181–276 | `run`: `inotify::poll_two(inotify.fd(), pipe.read_fd(), timeout)`, `read_events`, `HashMap<i32 (wd), PathBuf>` | Linux-only | thread body | ✓ S1.4 |
| 279–301 | `set_watches`: `add_watch(&dir, WATCH_MASK)`, `rm_watch(wd)` | Linux-only | `run` | ✓ S1.4 |

### git/status.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 41 | `use std::os::unix::ffi::OsStrExt` | Unix-only | `insert` | ✓ S1.16 |
| 279–306 | `status_blocking`: `Command::new("git")` with `-c core.hooksPath=/dev/null` (291), `.current_dir(root)` | Windows-differs | core:git/cache.rs:run_one ← `Git::start` ← app:app.rs:git | `/dev/null` is a Unix device path. §4 ✓ S1.13 ✓ P3.8 |
| 556–565 | `insert`: trailing `b'/'` means directory; `Path::new(OsStr::from_bytes(trimmed))`; `root.join(rel)` | Unix-only | `parse_porcelain_v2` ← `status_blocking` | git emits `/`-separated repo-relative paths ✓ S1.16 ✓ P3.8 |
| 577–587 | Rollup loop bounded by `ancestor.starts_with(root)` / `ancestor == root` | Windows-differs (case) | as above | §3 ✓ P3.19 |

### ops.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 73–98 | `normalize`: `path.is_absolute()`, else `current_dir().join(path)`; pushes `"/"` when empty (95) | Windows-differs | ~30 core callers incl. core:ops/delete.rs:check_deletable, core:ops/journal.rs:record, core:ops/link.rs:symlink, core:ops/paste.rs:carried, core:ops/trash.rs:trash, for_path, mount_point_of, core:sync/plan.rs:roots, walk | On Windows `is_absolute` needs a prefix and a root, so `\foo` and `/foo` are relative ✓ P3.12 |
| 109–120 | `is_url`: `use std::os::unix::ffi::OsStrExt`; scans `as_bytes()` for `://` | Unix-only | core:ops/paste.rs:carried; core:sync/plan.rs:roots | ✓ S1.16 ✓ P3.10 |
| 142–149 | `resolved`: `std::fs::canonicalize(path)` / `canonicalize(parent).join(name)` | Windows-differs | `is_ancestor_resolved` ← core:ops/paste.rs:plan_paste, core:sync/plan.rs:roots; `is_strict_ancestor_resolved` ← core:ops/copy.rs:copies_into_itself, move_path | Windows `canonicalize` returns `\\?\`-prefixed verbatim paths |
| 196–204 | `trim_trailing_slash`: `as_bytes()`, trims `b'/'`, `OsStr::from_bytes` | Unix-only | core:ops/delete.rs:remove_tree, remove_tree_unchecked | `\` is also a separator on Windows ✓ S1.16 ✓ P3.10 |
| 212–218 | `same_file`: `MetadataExt::dev()` / `ino()` of both `symlink_metadata` | Unix-only | core:ops/copy.rs:copy_tree_with, copy_file, move_path; core:ops/create.rs:rename; core:ops/paste.rs:plan_paste; core:sync/plan.rs:roots | ✓ S1.5 |

### ops/copy.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 54 | `const FICLONE: libc::c_ulong = 0x4004_9409` | Linux-only (macOS: compiles, ioctl fails) | `reflink` | ✓ S1.5 |
| 231–244 | `copy_symlink`: `std::os::unix::fs::symlink(&target, dst)` (239) | Unix-only | `copy_entry`; core:sync/execute.rs:copy_one | Windows needs `symlink_file`/`symlink_dir` plus privilege ✓ S1.5 |
| 432–439 | `sync_file`: `File::sync_all` | macOS-differs | `copy_file`, `sync_path` ← core:sync/rsync.rs:flush_here | Rust std on Apple implements `sync_all` with `fcntl(F_FULLFSYNC)`. Doc comment 431 describes Linux `fsync` |
| 464–479 | `sync_dir`: `File::open(dir)` then `sync_all`; `EINVAL` tolerated | Windows-differs | `sync_parent` ← `copy_symlink`, `copy_dir`, `copy_file`, core:sync/execute.rs:make_dir; core:sync/rsync.rs:flush_here | On Windows std's `File::open` of a directory fails (no `FILE_FLAG_BACKUP_SEMANTICS`) ✓ S1.5 |
| 473 | `e.raw_os_error() == Some(libc::EINVAL)` | Windows-differs | `sync_dir` | On Windows `raw_os_error` carries Win32 codes. `libc::EINVAL` there is the CRT's 22 ✓ S1.3 |
| 581–594 | `reflink`: `use std::os::unix::io::AsRawFd`; `libc::ioctl(writer, FICLONE, reader)` | Linux-only (macOS: compiles, always falls back; Windows: no compile) | `write_contents` ← `copy_file` ← `copy_entry`, `copy_file_with` | ✓ S1.5 |
| 629–635 | `apply_mode`: `PermissionsExt::mode()`, `Permissions::from_mode` | Unix-only | `copy_file`, `copy_dir`; core:sync/execute.rs:copy_all | ✓ S1.7 |
| 651–688 | `set_times`: `OsStrExt` → `CString`; `libc::timespec`; `libc::utimensat(AT_FDCWD, …, AT_SYMLINK_NOFOLLOW)` | Unix-only | `apply_times` ← `copy_file`, `copy_dir`; core:sync/execute.rs:copy_all | ✓ S1.5 |
| 655, 665–668 | `const UTIME_OMIT: i64 = 0x3ffffffe` used as `tv_nsec` for a missing time | Linux-only (macOS: compiles, wrong constant) | `set_times` | libc apple: `UTIME_OMIT = -2`. Taken for pre-1970 or unreadable atime/mtime ✓ S1.5 |
| 738–756 | `move_path`: `rename`, then `raw_os_error() == Some(libc::EXDEV)` → `move_cross_device` (740, 754) | Windows-differs | core:ops/paste.rs:execute; core:ops/journal.rs:undo_moves | A Windows cross-volume rename fails with Win32 `ERROR_NOT_SAME_DEVICE` (17). `libc::EXDEV` on the windows target is 18 ✓ S1.3 |
| 764–771 | `in_the_way`: `ENOTEMPTY \| EEXIST \| EISDIR \| ENOTDIR` against `raw_os_error()` | Windows-differs | `move_path` (746) | On the windows target `libc::EEXIST` = 17 = `ERROR_NOT_SAME_DEVICE`. A cross-volume move onto an existing destination therefore matches, and `move_path` then calls `remove_tree_unchecked(dst)` (751) ✓ S1.3 |

### ops/create.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 33–46 | `create`: `use std::os::unix::ffi::OsStrExt`; trailing `b'/'` means "directory"; trimmed with `OsStr::from_bytes` | Unix-only | app:app.rs:create | A typed trailing `\` is not a directory marker ✓ S1.16 ✓ P3.10 |
| 139–146 | `rename`: `from.parent().map(normalize) != to.parent().map(normalize)`; message falls back to `Path::new("/")` | Windows-differs (case) | app:app.rs:rename, run_bulk; app:bulk.rs:start | ✓ P3.18 |

### ops/delete.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 35 | `check_deletable`: `target == Path::new("/")` is the root rail | Windows-differs | `check_deletable_here` ← core:ops/jobs.rs:DeleteJob::run; core:ops/trash.rs:Trash::trash | Drive roots (`C:\`) are not caught by this rail ✓ P3.13 |
| 40–54 | Home and cwd rails via lexical `is_ancestor` | Windows-differs (case) | as above | |
| 63 | `current_dir().unwrap_or_else(\|_\| PathBuf::from("/"))` | Windows-differs | as above | ✓ P3.13 |
| 64 | `std::env::var_os("HOME")` | Windows-differs | as above | Unset on Windows, so the home rail is skipped ✓ S1.17 |
| 96–111 | `remove_tree`: a symlink is removed with `remove_file` (109) | Windows-differs | `delete_permanent` ← core:ops/jobs.rs:DeleteJob::run, core:ops/trash.rs:purge, core:sync/execute.rs:remove_extras, remove; core:ops/copy.rs:move_cross_device | Windows directory symlinks and junctions are removed with `remove_dir` ✓ P3.26 |
| 118–130 | `remove_tree_unchecked`: `remove_dir_all` for a real dir, otherwise `remove_file` | Windows-differs | core:ops/copy.rs:copy_tree_with, copy_symlink, copy_dir, move_path; core:ops/create.rs:rename; core:ops/trash.rs:trash | As above ✓ P3.26 |

### ops/journal.rs (in memory only; the journal is never written to disk)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 879 | `undo_link`: `std::fs::remove_file(link)` | Windows-differs | `undo_attempt` ← `Journal::undo` ← app:app.rs:undo | Directory symlinks need `remove_dir` on Windows ✓ P3.26 |
| 933 | `undo_links`: `std::fs::remove_file(&l.link)` | Windows-differs | as above | As above ✓ P3.26 |
| 1999 | `redo_links`: `std::os::unix::fs::symlink(text, &l.link)` | Unix-only | `Journal::redo` ← app:app.rs (redo) | Added 2026-09-29: redo postdates the inventory ✓ S1.5 |

### ops/link.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 40–70 | `relative_to`: strips the shared component prefix of two `normalize`d paths and pushes `..` per `Component::Normal` | Windows-differs | `symlink` (Relative) | Paths on different drives share no `Prefix`, so the remaining `Prefix`/`RootDir` components are pushed onto the result (63–65) |
| 85–88 | Link parent fallback `PathBuf::from("/")` | Windows-differs | `symlink` | ✓ P3.13 |
| 92 | `std::os::unix::fs::symlink(&text, link)` | Unix-only | app:app.rs:link_into | Windows: `symlink_file`/`symlink_dir`, needs privilege ✓ S1.5 |

### ops/mode.rs (added 2026-09-29: the permissions change postdates the inventory; lines as of 6aee8d1)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 63–64 | `std::os::fd::AsRawFd`; `std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}` | Unix-only | file-wide | ✓ S1.7, S1.19 |
| 313–390 | The anchored walk: `PROC_FD = "/proc/self/fd"`, `proc_ready`, `named`, `open_named` with `libc::O_PATH \| O_NOFOLLOW \| O_DIRECTORY`, `open_in`, `stat_in` | Linux-only (macOS: no `O_PATH`, no `/proc`; Windows: neither) | `plan`, `chmod`, `undo`, `redo` ← app:app.rs, app:app/permissions.rs | ✓ S1.19 |
| 423–483 | `Finder` (anchor opened with `O_PATH \| O_DIRECTORY`, `ENOTDIR` for a swapped folder) | Linux-only | as above | ✓ S1.19 |
| 486–489 | `swapped`: `ENOTDIR \| ELOOP` | Linux-only numbers | `lookup_failed`, `unreachable`, `redo_unreachable` | ✓ S1.19 |
| 503–516 | `set_mode_of`: `set_permissions` of the `/proc` name with `Permissions::from_mode` | Linux-only | `set_one`, `undo`, `redo` | ✓ S1.19 |
| 564–634, 825–844, 1054, 1085–1145 | `meta.mode()`, `dev()`, `ino()` | Unix-only | `plan`, `set_one`, `redo`, `still_as_*` | ✓ S1.7 |
| 819 | `e.raw_os_error() == Some(libc::ENOTDIR)` | Unix-only numbers | `set_one` | ✓ S1.3 |
| 1181–2073 | Tests set modes with `from_mode`, make links with `std::os::unix::fs::symlink`, and read modes back | Unix/Linux semantics | | Disk tests moved to a Linux-gated `on_disk` module ✓ S1.19 |

### ops/paste.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 128–176 | `Clipboard::contains` / `toggle`: `PathBuf` equality and `HashSet<PathBuf>` | Windows-differs (case) | app:app.rs:toggle_yank, overlay_command | ✓ P3.20 |
| 400, 457, 462 | `claimed.contains(&dst)` / `&candidate` in `plan_paste` and `unique_name` | Windows-differs (case) | `plan_paste` ← app:app.rs:paste_into; `unique_name` ← app:app.rs:make_folder, remote_download; core:archive/unpack.rs:destinations; core:vfs/mod.rs:unique_name | On a case-insensitive volume, `A.txt` and `a.txt` are one slot ✓ P3.20 |

### ops/trash.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 1–30 | Module: freedesktop.org Trash spec (`Trash/files`, `Trash/info/*.trashinfo`, `$topdir/.Trash[-$uid]`) | Linux-only | | The macOS Finder trash is `~/.Trash` (plus `/Volumes/*/.Trashes/<uid>`). The Windows Recycle Bin is per-volume `$Recycle.Bin\<SID>` ✓ S1.6 |
| 110–116 | `Trash::home`: `var_os("XDG_DATA_HOME")`, `var_os("HOME")` (error if unset) | Linux-only | app:app.rs:show_trash; `for_path`; core:sync/mod.rs:trash_available, trash_for | Windows: `HOME` unset gives `Err("$HOME is not set: no home trash")` ✓ S1.6 |
| 132–145 | `ensure`: `PermissionsExt`, `from_mode(0o700)` | Unix-only | `Trash::trash` | ✓ S1.6 |
| 178–184 | `trash`: `rename(path, &dst)`, `Some(libc::EXDEV)` → `move_cross_device` | Windows-differs | `Trash::trash` ← core:ops/jobs.rs:TrashJob::run; core:sync/execute.rs:remove; app:app.rs:run, menu_action | See ops/copy.rs:738 on Win32 codes ✓ S1.3 |
| 201–205 | `claim_name`: `room = MAX_NAME_BYTES (255) - "trashinfo".len() - 1`, in bytes | Windows-differs | `trash` | The NTFS component limit is 255 UTF-16 units ✓ S1.6 ✓ P3.6 |
| 255–270 | `list`: `use std::os::unix::ffi::{OsStrExt, OsStringExt}`; strips `.trashinfo` on bytes; `OsString::from_vec` | Unix-only | app:app.rs:show_trash | ✓ S1.6 |
| 410–415 | `restorable_destination`: `original.is_absolute()` and no `..` | Windows-differs | `restore` | A decoded `/home/…` is not absolute on Windows, so the restore is refused ✓ S1.6 |
| 462–468 | `restore`: `rename`, `Some(libc::EXDEV)` → `move_cross_device` | Windows-differs | `Trash::restore`; core:ops/journal.rs:undo_trash; app:app.rs:trash_restore | ✓ S1.3 |
| 484–491 | `home_trash_path`: `dir.is_absolute()`; `home.join(".local/share/Trash")` | Linux-only | `Trash::home` | ✓ S1.6 |
| 494–521 | `for_path`: compares `device_of(target)` with `device_of(home)`, else `mount_point_of` + `topdir_trash(uid())` | Linux-only | core:ops/jobs.rs:TrashJob::run | ✓ S1.6 |
| 524–551 | `topdir_trash`: `.Trash` must be a real dir with the sticky bit (`mode() & 0o1000`); `.Trash/<uid>`; `.Trash-<uid>` at 0o700 | Linux-only | `for_path`; core:sync/mod.rs:trash_for | ✓ S1.6 |
| 554–564 | `device_of`: `MetadataExt::dev()` of the nearest existing ancestor | Unix-only | `for_path`, `mount_point_of`; core:sync/mod.rs:trash_available, trash_for | ✓ S1.6 |
| 568–582 | `mount_point_of`: walks `parent()` until `device_of` changes | Unix-only | `for_path`; core:sync/mod.rs:trash_available, trash_for | Windows mount points and junctions do not change a `dev` ✓ S1.6 |
| 588–594 | `uid()`: `libc::getuid()` | Unix-only | `for_path`; core:sync/mod.rs:trash_available, trash_for; app:mounts.rs:gvfs_root | ✓ S1.8 |
| 609–633 | `fit`: `use std::os::unix::ffi::{OsStrExt, OsStringExt}`; stem/extension clipped as bytes | Unix-only | `suffixed` ← core:ops/paste.rs:unique_name, core:vfs/mod.rs:unique_name, app:remote.rs:free_name; `claim_name` | ✓ S1.16 ✓ P3.6 |
| 637–646 | `clip`: cuts at a UTF-8 boundary in a byte string | Unix-only (byte model) | `fit` | ✓ S1.16 ✓ P3.6 |
| 679–691 | `encode_path`: `OsStrExt::as_bytes`, percent-encodes, `/` unreserved | Unix-only | `trashinfo_text` ← `Trash::trash` | ✓ S1.6 |
| 695–720 | `decode_path`: `OsStringExt::from_vec` | Unix-only | `parse_trashinfo` ← `Trash::list` | ✓ S1.6 |

### preview/cache.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 128–132 | `cache_dir`: `std::env::temp_dir()` + `yazi-{uid()}` | Unix-only (uid) | `thumb_path` ← `cached_thumb` ← app:grid.rs:tile_image, core:preview/job.rs:build | `temp_dir()` is `$TMPDIR` (per-user `/var/folders/…`) on macOS and `%TEMP%` on Windows ✓ S1.8 |
| 137 | `cache_key`: `use std::os::unix::fs::MetadataExt` | Unix-only | `thumb_path`, `store_thumb` | ✓ S1.7 |
| 143 | `path.hash(&mut h)` via std `Hash for Path` | Windows-differs | `cache_key` | The bytes std's `Path` hash feeds in are platform-specific |
| 149–151 | ctime from `meta.ctime()`, `meta.ctime_nsec()` | Unix-only | `cache_key` | ✓ S1.7 |
| 195–204 | `store_thumb`: `temp_dir()` + `yazi-{uid()}`, `create_dir_all` | Unix-only (uid) | app:preview/decode.rs:write_thumb | ✓ S1.8 |
| 206–212 | `uid()`: `libc::getuid()` | Unix-only | `cache_dir`, `store_thumb` | ✓ S1.8 |

### rename/facts.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 53–88 | `Civil::local`: `libc::time_t`, `libc::tm` (`mem::zeroed`), `libc::localtime_r` | Unix-only (Windows libc has `localtime_s`, not `localtime_r`) | `Civil::now` ← app:bulk.rs:from_facts, preview, rewrite, refresh, expand_tokens, accept; `Facts::stat` ← app:bulk.rs:new | ✓ S1.10 |

### state/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 221–223 | `state_path()` → `state_path_from(var_os("XDG_STATE_HOME"), var_os("HOME"))` | Linux-only | `StateStore::load` ← app:app.rs:new | Windows: `HOME` unset → `None` → session-only ✓ S1.11 |
| 481, 500 | `render`: `escape(path_bytes(key))`, tab paths `path_bytes(tab)` | Unix-only | `flush` ← app:app.rs:flush_state | ✓ S1.16 |
| 536 | `parse`: `if !key.starts_with(b"/")` → line skipped | Windows-differs | `load_from` ← `load` | A Windows absolute path starts with a drive letter, so every directory record would be dropped ✓ P3.5 |
| 594, 616 | `parse` / `parse_tabs`: `path_from(&key)`, `path_from(&value)` | Unix-only | as above | ✓ S1.16 |
| 637–657 | `state_path_from`: `$XDG_STATE_HOME/delightfile/state` or `$HOME/.local/state/delightfile/state` | Linux-only; macOS-differs (not `~/Library`) | `state_path`; app:portal/request.rs:from_env | ✓ S1.11 |
| 744–747 | `path_bytes`: `OsStrExt::as_bytes` | Unix-only | `render` | ✓ S1.16 |
| 749–752 | `path_from`: `OsStringExt::from_vec` | Unix-only | `parse`, `parse_tabs` | ✓ S1.16 |
| 202, 305–318 | `dirs: HashMap<PathBuf, Record>`; `get`/`sort`/`linemode`/`show_hidden` | Windows-differs (case) | app:app.rs, app:tab.rs:new, app:app/places.rs | ✓ P3.19 |

### state/pins.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 59–61 | `Pin::expanded_path` → `expand_home` (`HOME`) | Windows-differs | app:app/places.rs:pool; app:finder.rs:merge_places | ✓ P3.14 |
| 92–94 | `same_place`: `Path::new(&expand_home(a)) == Path::new(&expand_home(b))` | Windows-differs (case) | `Pin::is` ← `pinned`, `unpin`, `set_pin_key` ← app:app/places.rs | ✓ P3.19 |

### sync/execute.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 470–482 | `forget_cached`: `use std::os::unix::io::AsRawFd`; `libc::posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)` | Linux-only (macOS: no compile, libc apple has no `posix_fadvise`) | `read_back` ← `Run::same_bytes` (verify); core:sync/rsync.rs:verify_remote | ✓ S1.5 |

### sync/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 267–282 | `SyncPlan::label`: `format!("{name}/{}", item.rel.to_string_lossy())`, trailing `'/'` for folders | Windows-differs | app:sync.rs (card rows) | Mixed separators on Windows (`rel` renders with `\`) ✓ P3.17 |
| 310–318 | `is_debris`: `use std::os::unix::ffi::OsStrExt`; `name.as_bytes().starts_with(TEMP_PREFIX)` | Unix-only | `has_debris`, `in_sync`, `listed`, `removals`; core:sync/execute.rs:remove_extras | ✓ S1.16 ✓ P3.9 |
| 412–436 | `trash_available`: `Trash::home`, `device_of`, `mount_point_of`, `.Trash` sticky `mode() & 0o1000` (427), `.Trash-{uid()}` (431), `writable` | Linux-only | core:sync/plan.rs:walk | ✓ S1.6 |
| 414 | `use std::os::unix::fs::PermissionsExt` | Unix-only | `trash_available` | ✓ S1.6 |
| 443–458 | `trash_for`: home trash if same `dev`, else `topdir_trash(mount_point_of(dest), uid())` | Linux-only | core:sync/execute.rs:remove | ✓ S1.6 |
| 479–488 | `writable`: `OsStrExt` → `CString`; `libc::access(path, libc::W_OK)` | Unix-only (windows libc has `access`, not `W_OK`) | `trash_available` | ✓ S1.5 |

### sync/plan.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 51–67 | `roots`: duplicate yanked names via `HashSet<OsString>` | Windows-differs (case) | `walk` ← `plan` ← app:sync.rs:plan_job | ✓ P3.19 |
| 178–199 | `visit`: `symlink_metadata(dst)` decides New/Changed/Unchanged | Windows-differs (case) | as above | On a case-insensitive destination, `dst.join(name)` resolves to a differently cased existing entry ✓ P3.18 |
| 366–424 | `case_twins`, `fold`, `fold_char`: Unicode simple case folding of UTF-8 names (`to_str()`; non-UTF-8 skipped) | Windows-/macOS-relevant | `extras_in` | Already handles case-folding destinations. No Unicode normalization (NFC/NFD) handling |

### sync/rsync.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 43 | `use std::os::unix::ffi::{OsStrExt, OsStringExt}` | Unix-only | file-wide | ✓ S1.16 |
| 69–77 | `available()`: `rsync --version` | Windows-differs; macOS-differs | app:app/syncing.rs:remote_sync | rsync is not in a stock Windows install. macOS bundles `/usr/bin/rsync` 2.6.9 (macOS 15 adds openrsync) ✓ S1.14 |
| 126–139 | `Host::shell`: `Command::new("ssh")` + a POSIX-shell script for the server | Windows-differs | `remote_digests` | §4 |
| 173–188 | `rsh()` / `rsh_with("ssh")`: the `-e` string rsync uses to start ssh | Windows-differs | `common()` → `dry_run_args`, `run_args` | |
| 230–244 | `endpoint`: `as_os_str().as_bytes()`, trims or appends `b'/'`, prefixes `host:` for the remote side, `OsStr::from_bytes` | Unix-only | `endpoints` → `dry_run_args`, `run_args` | In rsync's argument syntax a colon before the first `/` means `host:path`, so a Windows local path `C:\x` reads as host `C` ✓ S1.16 ✓ P3.9 |
| 266–308 | `dry_run_args` / `run_args`: `--info=progress2`, `--no-inc-recursive`, `--out-format=%i %l %n`, `--delete-after`, `--fsync` | macOS-differs | `plan`, `run` | `--info` needs rsync ≥ 3.1.0 and `--no-inc-recursive` ≥ 3.0.0. `--fsync` is gated on ≥ 3.2.0 (`FSYNC_SINCE`, 312) |
| 383–426 | `parse_line`: `OsString::from_vec(name)` from rsync's itemized bytes | Unix-only | `parse_itemized` ← `plan`; stdout thread in `run` | ✓ S1.16 ✓ P3.9 |
| 429–450 | `unescape`: rsync `\#ooo` octal escapes to raw bytes | Unix-only (byte model) | `parse_line` | ✓ P3.9 |
| 511–536 | `plan`: `Command::new("rsync")` dry run via `collect` | Windows-differs | app:sync.rs:remote_plan_job | §4 |
| 631–658 | `wait`: on stop, `signal(child, libc::SIGCONT)` then `child.kill()` | Unix-only | `collect`, `run` | ✓ S1.13 |
| 662–675 | `signal`: `libc::pid_t::try_from(child.id())`, `libc::kill(pid, signal)` | Unix-only (Windows libc: no `kill`, no `pid_t`) | `wait`, `run` | ✓ S1.13 |
| 914–992 | `run`: `Command::new("rsync")`; pause/resume as `libc::SIGSTOP` / `libc::SIGCONT` (966–976) | Unix-only | `execute` ← core:sync/execute.rs:execute ← app:sync.rs:sync_job | ✓ S1.13 |
| 1024–1042 | `far_side`: server paths held as `PathBuf` (`name.join(&item.rel)`, `source.parent()`) | Windows-differs | `verify_remote` | `PathBuf::join` inserts `\` on Windows, and these paths go to a Unix server |
| 1143–1148 | `remote_digests`: stdin = NUL-separated `name.as_os_str().as_bytes()` | Unix-only | `verify_remote` | ✓ S1.16 ✓ P3.9 |
| 1156 | Exit-status check `matches!(status.code(), Some(255) \| Some(126) \| Some(127) \| None)` | Windows-differs | `remote_digests` | `code()` is `None` only for a signal death on Unix |
| 1189–1211 | `parse_sha256sum`: `OsString::from_vec(name)` | Unix-only | `verify_remote` | ✓ S1.16 ✓ P3.9 |

### tasks.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 349–363 | `default_transient`: `raw_os_error()` in `{11, 16, 23, 24, 26, 116}` (EAGAIN, EBUSY, ENFILE, EMFILE, ETXTBSY, ESTALE on Linux) | Linux-only (macOS: compiles, wrong numbers) | `Job::is_transient` default ← `run_one` (982) | libc apple: EAGAIN=35, ESTALE=70, and 11 is EDEADLK. Windows codes are Win32 ✓ S1.3 |
| 896 | `worker_loop`: `crate::thread::lower_priority(NICE_BULK)` | macOS-differs (see thread.rs) | task pool workers | ✓ S1.9 |

### test_support.rs (compiled under `cfg(test)` and under feature `test-support`, which df-app enables as a dev-dependency)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 61–68 | `TempTree::symlink`: `std::os::unix::fs::symlink(target, &p)` | Unix-only | df-core tests; df-app tests (feature) | df-app's test build does not compile on Windows through this ✓ S1.15 |
| 81–95 | `gnarly_names()`: names with `\n`, `\t`, `\\`, `"`, and 255 `x` | Windows-differs | ops tests; rename/template tests; df-app tests | On Windows `\` is a separator and `"`, `\n`, `\t` are illegal in names |

### thread.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 27–43 | `lower_priority`: `libc::setpriority(libc::PRIO_PROCESS, 0, nice)` | Linux-only (macOS: compiles, whole process; Windows: no compile) | core:tasks.rs:worker_loop; core:du/scanner.rs:new; core:preview/job.rs:with_debounce; app:preview/body.rs, app:bulk.rs, app:grid.rs, app:preview/decode.rs, app:preview/prepare.rs, app:playback/mod.rs, app:preview/doc/mod.rs (one call each, inside spawned worker closures) | The doc comment (19–21) states that `who = 0` means the calling thread only on Linux ✓ S1.9 |

### vfs/config.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 144–151 | `Service::key_path`: `~` → `var_os("HOME")` + `format!("{}{rest}", …)` | Windows-differs | `command`; app:app/syncing.rs:remote_sync | ✓ S1.17 ✓ P3.14 |
| 181–202 | `Service::command`: `Command::new("ssh")` `-x -o BatchMode=yes -o ConnectTimeout=15 [-p] [-i] -s <dest> sftp` | Windows-differs | core:vfs/conn.rs:Transport::spawn | §4 |
| 292–308 | `config_paths` / `xdg_config_home`: `$XDG_CONFIG_HOME` or `$HOME/.config` → `yazi/vfs.toml`, plus `config_dir()/vfs.toml` | Windows-differs; macOS-differs | `VfsConfig::load` ← core:vfs/mod.rs:Vfs::start ← app:app.rs:vfs | ✓ S1.11 |

### vfs/conn.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 54 | `use std::os::unix::fs::FileExt` (for `write_all_at`) | Unix-only | `download_body` | ✓ S1.5 |
| 55 | `use std::os::unix::io::AsRawFd` | Unix-only | `Transport` | ✓ S1.12 |
| 184–188 | `Transport::spawn`: `poll::set_nonblocking(stdin.as_raw_fd())`, `(stderr.as_raw_fd())` | Unix-only | `Connection::connect` ← core:vfs/mod.rs worker (`dispatch`) | ✓ S1.12 |
| 256–268 | `drain_stderr`: `poll::poll_read2(self.stderr.as_raw_fd(), -1, left)` | Unix-only | `fill`, `write_all` | A negative fd is ignored by `poll` ✓ S1.12 |
| 271–308 | `fill`: `poll_read2(stdout fd, stderr fd or -1, left)` then a blocking `read` | Unix-only | `read_packet` | ✓ S1.12 |
| 337–367 | `write_all`: `poll::poll_write(self.stdin.as_raw_fd(), left)` then a non-blocking `write` | Unix-only | `send`, `connect` | ✓ S1.12 |
| 897 | `download_body`: `file.write_all_at(&data, offset)` | Unix-only | `download` ← core:vfs/mod.rs:Vfs::download, download_to_temp ← app:app.rs:open_remote, sync_remote_preview, remote_download | Windows `FileExt` has `seek_write`, not `write_all_at` ✓ S1.5 |

### vfs/child.rs (added 2026-09-29: the rclone daemon postdates the inventory; lines as of 6aee8d1)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 55 | `use std::os::unix::process::CommandExt` (`pre_exec`) | Unix-only | `tie_to_this_thread` | ✓ S1.50 |
| 61–95 | `tie_to_this_thread`: `prctl(PR_SET_PDEATHSIG, SIGTERM)` and `getppid()` in `pre_exec`, `ESRCH` when the parent is gone | Linux-only (macOS: no `prctl`; Windows: no `pre_exec`) | core:vfs/rclone.rs:Daemon::spawn; app:mounts.rs (gvfs watcher) | ✓ S1.50 |
| 104–116 | `terminate`: `libc::kill(pid, SIGTERM)` on a child not yet reaped | Unix-only | core:vfs/rclone.rs:Daemon drop | ✓ S1.50 |

### vfs/http.rs (added 2026-09-29; lines as of 6aee8d1)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 32, 95–101 | `use std::os::unix::net::UnixStream`; `post` connects and sets read and write timeouts | Unix-only (Windows: no unix sockets in `std`) | `post` ← core:vfs/rclone.rs:Daemon::call, run_job | ✓ S1.51 |
| 308, 455–560 | Tests serve one response on a `UnixListener` | Unix-only | | `#[cfg(unix)] mod over_a_socket` ✓ S1.51 |

### vfs/rclone.rs (added 2026-09-29; lines as of 6aee8d1)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 98 | `use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt}` | Unix-only | `private_dir`, a test | ✓ S1.51 |
| 1091–1133 | `socket_path`: `$XDG_RUNTIME_DIR` (non-empty), `$TMPDIR/delightfile-<uid>`, `/tmp/delightfile-<uid>`, `sun_path` bound | Unix model (Windows: no unix socket) | `Daemon::spawn` | uid ✓ S1.8, runtime dir ✓ S1.11 |
| 1140–1157 | `private_dir`: `DirBuilder::mode(0o700)`, owner `uid()` check, re-close with `from_mode(0o700)` | Unix-only | `socket_path` | ✓ S1.51 |
| 248, 857 | `child::tie_to_this_thread`, `child::terminate` | Linux-only / Unix-only | `Daemon::spawn`, `Drop for Daemon` | ✓ S1.50 |

### vfs/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 657–668 | `download_to_temp`: `temp_dir()/delightfile-vfs-{pid}/{n}-{remote.name()}` | Windows-differs | app:app.rs:open_remote, sync_remote_preview | A remote name is used as a local file name unchanged (Windows-illegal characters and reserved names are possible) |
| 694–715 | `Vfs::unique_name`: `OsString::from(remote.name())` → `trash::suffixed` (Unix-only `fit`) | Unix-only (via ops/trash.rs:609) | `upload_new` ← app:app.rs:spawn_upload | |
| 1174 | `remote_entry`: `path: PathBuf::from(dir.join(&name).to_url())` (an `sftp://` URL inside a `PathBuf`) | Windows-differs | `convert_batch`, `stat_entry` ← app:app.rs:remote_upload | §3.7 |

### vfs/poll.rs (the whole file)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 70–97 | `poll_read2`: `libc::pollfd` ×2, `libc::POLLIN`, `libc::poll` | Unix-only (Windows: no `poll`; pipes are not pollable handles) | core:vfs/conn.rs:drain_stderr, fill | ✓ S1.12 |
| 105–122 | `poll_write`: `libc::pollfd`, `libc::POLLOUT`, `libc::poll` | Unix-only | core:vfs/conn.rs:write_all | ✓ S1.12 |
| 132–145 | `set_nonblocking`: `libc::fcntl(F_GETFL)`, `fcntl(F_SETFL, flags \| O_NONBLOCK)` | Unix-only | core:vfs/conn.rs:Transport::spawn | ✓ S1.12 |

### zoxide/mod.rs

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| 117–126 | `db_path`: `$_ZO_DATA_DIR/db.zo`, else `$XDG_DATA_HOME` or `$HOME/.local/share`, then `zoxide/db.zo` | Linux-only; macOS-differs | `load` ← app:app.rs:zoxide_db | Upstream zoxide resolves its data dir with `dirs::data_local_dir()` (macOS: `~/Library/Application Support`; Windows: `%LOCALAPPDATA%`) when `_ZO_DATA_DIR` is unset ✓ S1.11 |
| 338–364 | `classify`: last component via `path.rsplit('/')` | Windows-differs | `query` ← app:app.rs:jump_rows | Windows zoxide paths use `\` ✓ P3.16 |

---

## 2. Public surfaces that a platform layer must preserve

Signatures are verbatim. Each item is preceded by the first line of its doc comment, and the `// L` suffix is the source line. `#[cfg(test)]` items are marked. Private functions that hold the platform-specific body are listed under "private" where the port would replace them.

### fs::watch (+ fs::inotify)

Re-exported at fs/mod.rs:81 as `pub use watch::{WatchEvent, Watcher, DEBOUNCE};`. The module `fs::inotify` is private (`mod inotify;`, fs/mod.rs:51), so its `pub` items are visible to `fs::watch` only.

```rust
// src/fs/watch.rs
/// How long a burst is allowed to accumulate before the pane is refreshed.
pub const DEBOUNCE: Duration = Duration::from_millis(80);   // L52
/// What the watcher tells the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Changed(PathBuf),
    Gone(PathBuf),
    Overflow,
}   // L56
/// A running inotify watcher, or nothing at all.
pub struct Watcher {
    control: Sender<Control>,
    events: Receiver<WatchEvent>,
    pipe: Option<Arc<Pipe>>,
    thread: Option<std::thread::JoinHandle<()>>,
}   // L82
impl Watcher {
    /// Start watching. `notify` is rung whenever events become available, the
    pub fn new(notify: Notifier) -> io::Result<Watcher>   // L94
    /// Start watching, or return a watcher that does nothing.
    pub fn start(notify: Notifier) -> Watcher   // L118
    /// A watcher that watches nothing and emits nothing.
    pub fn disabled() -> Watcher   // L129
    /// Whether this watcher is actually watching.
    pub fn is_active(&self) -> bool   // L141
    /// Set the watched directories to exactly these — typically the list's
    pub fn watch(&self, dirs: Vec<PathBuf>)   // L147
    /// The channel, for a caller that wants to select on it.
    pub fn events(&self) -> &Receiver<WatchEvent>   // L154
    /// Everything that has arrived, without blocking.
    pub fn drain(&self) -> Vec<WatchEvent>   // L159
}
impl Drop for Watcher   // L170: sends Control::Stop, wakes the pipe, joins the thread
// private: enum Control { Watch(Vec<PathBuf>), Stop } (L69); fn run(inotify: Inotify, pipe: Arc<Pipe>, control: Receiver<Control>, events: Sender<WatchEvent>, notify: Notifier) (L181); fn set_watches(inotify: &Inotify, watched: &mut HashMap<i32, PathBuf>, dirs: Vec<PathBuf>) (L279)
```

```rust
// src/fs/inotify.rs  (module-private; `#![allow(unsafe_code)]` file-wide)
/// The events a directory listing cares about.
pub const WATCH_MASK: u32 = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO | libc::IN_ATTRIB | libc::IN_CLOSE_WRITE | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_ONLYDIR;   // L46
/// One event, with the name copied out of the kernel's buffer.
pub struct Event {
    pub wd: i32,
    pub mask: u32,
    pub name: Option<String>,
}   // L58
impl Event {
    /// The kernel's event queue filled up and events were dropped. There is no
    pub fn is_overflow(&self) -> bool   // L71
    /// The watched directory itself went away.
    pub fn is_self_gone(&self) -> bool   // L76
}
/// An inotify instance. Closes its descriptor on drop, which also drops every
pub struct Inotify {
    fd: i32,
}   // L84
impl Inotify {
    /// `IN_NONBLOCK` so a `read` after a `poll` can never stall the watcher
    pub fn new() -> io::Result<Inotify>   // L92
    pub fn fd(&self) -> i32   // L101
    /// Watch a directory. Returns the watch descriptor, which the caller maps
    pub fn add_watch(&self, path: &Path, mask: u32) -> io::Result<i32>   // L108
    pub fn rm_watch(&self, wd: i32)   // L120
    /// Drain whatever is queued. Returns an empty vector when nothing is ready
    pub fn read_events(&self) -> io::Result<Vec<Event>>   // L129
}
/// A self-pipe: the standard way to wake a thread out of `poll` from another
pub struct Pipe {
    read_fd: i32,
    write_fd: i32,
}   // L205
impl Pipe {
    pub fn new() -> io::Result<Pipe>   // L211
    pub fn read_fd(&self) -> i32   // L224
    /// Wake whoever is polling the read end. Safe to call from any thread and
    pub fn wake(&self)   // L231
    /// Throw away pending wake-up bytes so the next `poll` blocks again.
    pub fn drain(&self)   // L244
}
/// Wait until either descriptor is readable, or `timeout` elapses.
pub fn poll_two(a: i32, b: i32, timeout: Option<std::time::Duration>) -> io::Result<(bool, bool)>   // L280
```

Facts: the watch unit is one directory, with no recursion. The watched set is replaced on every `watch()` call. Event names are converted with `String::from_utf8_lossy` (186). Debounce is a fixed deadline 80 ms after the first event of a burst.

### ops::trash

```rust
// src/ops/trash.rs   (ops.rs:63 re-exports `purge, Trash, TrashedItem`)
/// The suffix on every info file, per the spec.
pub const TRASHINFO_EXT: &str = "trashinfo";   // L42
/// How many `name_1`, `name_2`… variants to try before giving up on a name
pub const MAX_TRASH_COLLISIONS: u32 = 1000;   // L50
/// The longest a single path component may be on ext4/btrfs/xfs. Names are
pub const MAX_NAME_BYTES: usize = 255;   // L55
/// One trashed thing, and everything needed to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    pub trash_root: PathBuf,
    pub name: OsString,
    pub original: PathBuf,
    pub deleted_at: String,
}   // L62
impl TrashedItem {
    pub fn files_path(&self) -> PathBuf   // L75
    /// Whether this is a file in `files/` that no `.trashinfo` describes.
    pub fn is_orphan(&self) -> bool   // L84
    pub fn info_path(&self) -> PathBuf   // L88
}
/// A trash directory: `…/Trash`, with `files/` and `info/` inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    root: PathBuf,
}   // L98
impl Trash {
    /// A trash rooted at an explicit directory. The whole spec works the same
    pub fn at(root: impl Into<PathBuf>) -> Trash   // L105
    /// The user's home trash, from `$XDG_DATA_HOME` or `$HOME/.local/share`.
    pub fn home() -> Result<Trash>   // L110
    pub fn root(&self) -> &Path   // L118
    pub fn files_dir(&self) -> PathBuf   // L122
    pub fn info_dir(&self) -> PathBuf   // L126
    /// Make `files/` and `info/` if they are missing. Mode 0700: a trash is
    pub fn ensure(&self) -> Result<()>   // L132
    /// Move `path` into this trash.
    pub fn trash(&self, path: &Path, ctx: &TaskCtx) -> Result<TrashedItem>   // L153
    /// Every item currently in this trash, for the `trash://` view and for
    pub fn list(&self) -> Result<Vec<TrashedItem>>   // L246
    /// Put an item back where it came from.
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf>   // L340
    /// Destroy one item for good — the trash view's `D` (PLAN §7.4).
    pub fn purge(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<()>   // L345
}
/// Destroy a trashed item for good: its record *and* its file.
pub fn purge(item: &TrashedItem, ctx: &TaskCtx) -> Result<()>   // L380
/// Put a trashed item back where it came from — the inverse `u` runs.
pub fn restore(item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf>   // L422
/// Where the home trash lives, given the two environment variables. Pure, so
pub fn home_trash_path(xdg_data_home: Option<&Path>, home: &Path) -> PathBuf   // L484
/// Pick the trash `path` should go into (spec §Trash directories).
pub fn for_path(path: &Path) -> Result<Trash>   // L494
/// The spec's two candidates under a mount point, in order.
pub fn topdir_trash(topdir: &Path, uid: u32) -> Result<PathBuf>   // L524
/// The device number of a path, or of the nearest ancestor that exists.
pub(crate) fn device_of(path: &Path) -> Option<u64>   // L554
/// Walk up until the device number changes: the last path with the original
pub fn mount_point_of(path: &Path) -> Option<PathBuf>   // L568
/// The process's user id. Public because it is also what names the
pub fn uid() -> u32   // L588
/// `name_1`, `name_2`… — the yazi collision suffix, applied before the
pub fn suffixed(name: &OsStr, n: u32) -> OsString   // L598
/// The contents of a `.trashinfo` file.
pub fn trashinfo_text(original: &Path, deleted_at: &str) -> String   // L649
/// Read back `Path` and `DeletionDate` from a `.trashinfo` file.
pub fn parse_trashinfo(text: &str) -> Result<(PathBuf, String)>   // L658
/// Percent-encode a path for `Path=`, per the spec's reference to RFC 2396.
pub fn encode_path(path: &Path) -> String   // L679
/// The inverse of [`encode_path`]. Invalid escapes are kept verbatim: a
pub fn decode_path(text: &str) -> PathBuf   // L695
/// `YYYY-MM-DDThh:mm:ss` in UTC.
pub fn iso8601_utc(t: SystemTime) -> String   // L729
// private: fn claim_name(&self, base: &OsStr) -> Result<(OsString, PathBuf)> (L201); fn orphans(&self, known: &[TrashedItem]) -> Result<Vec<TrashedItem>> (L307); fn restorable_destination(original: &Path) -> bool (L410); fn fit(name: &OsStr, suffix: Option<u32>, max: usize) -> OsString (L609); fn clip(bytes: &[u8], room: usize) -> Vec<u8> (L637); fn civil_from_days(days: i64) -> (i64, u32, u32) (L747)
```

Facts: `TrashedItem` is carried in `OpRecord::Trash` (ops/journal.rs:437). df-app reads it in the `trash://` view (app:trashview.rs) and constructs it only in tests (app:tab.rs:1402, app:app.rs:19209). `uid()` is used by df-app for `/run/user/<uid>` (app:mounts.rs:gvfs_root).

### ops::copy (reflink, move and metadata-copy parts)

```rust
// src/ops/copy.rs   (ops.rs:47 re-exports copy_tree, copy_tree_with, measure, move_cross_device, move_path, CopyOptions, CopyStats, COPY_CHUNK)
/// How much of a file to move between two cancellation checkpoints.
pub const COPY_CHUNK: usize = 1 << 20;   // L47
/// How a copy writes: whether it may replace what is in the way, and whether
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyOptions {
    pub overwrite: bool,
    pub durable: bool,
}   // L66
/// What a copy did, for the toast and for the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyStats {
    pub files: u64,
    pub bytes: u64,
    pub skipped: Vec<PathBuf>,
}   // L85
/// Total bytes and entries under `path`, for a progress bar that means
pub fn measure(path: &Path) -> Result<(u64, u64)>   // L108
/// Copy anything to anywhere: a file, a symlink (recreated, never followed) or
pub fn copy_tree(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<CopyStats>   // L139
/// [`copy_tree`], with the durability choice as well as the overwrite one.
pub fn copy_tree_with(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
) -> Result<CopyStats>   // L152
/// Recreate a symlink, target text and all. Never followed: copying a link to
pub(crate) fn copy_symlink(src: &Path, dst: &Path, options: CopyOptions) -> Result<()>   // L231
/// Copy one regular file and nothing else: the entry point for a caller that
pub(crate) fn copy_file_with(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
) -> Result<u64>   // L418
/// `fsync(2)` a file that is already written and closed, by opening it again:
pub(crate) fn sync_path(path: &Path) -> Result<()>   // L444
/// `fsync(2)` the directory `path` is in, so the name it was just given — a
pub(crate) fn sync_parent(path: &Path) -> Result<()>   // L451
/// `fsync(2)` a directory: open it read-only, flush, close.
pub(crate) fn sync_dir(dir: &Path) -> Result<()>   // L464
#[cfg(test)] pub(crate) fn syncs() -> (u64, u64)   // L491
#[cfg(test)] pub(crate) fn take_events() -> Vec<String>   // L509
/// How every temporary name a copy makes begins. Public to the crate because
pub(crate) const TEMP_PREFIX: &str = ".df-tmp-";   // L516
#[cfg(test)] pub(crate) fn without_reflink<T>(f: impl FnOnce() -> T) -> T   // L619
/// Copy the permission bits. A failure is logged, never fatal: a copy onto a
pub(crate) fn apply_mode(path: &Path, meta: &std::fs::Metadata)   // L629
/// Copy access and modification times. Same rule as [`apply_mode`]: advisory.
pub(crate) fn apply_times(path: &Path, meta: &std::fs::Metadata)   // L638
/// Move `src` to `dst`: `rename(2)`, falling back to copy-verify-delete across
pub fn move_path(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<()>   // L699
/// The cross-filesystem move: copy everything, prove it arrived, then delete
pub fn move_cross_device(src: &Path, dst: &Path, ctx: &TaskCtx) -> Result<()>   // L779
/// Prove that `dst` reproduces `src`: same kinds, same sizes, same link
pub fn verify_copy(src: &Path, dst: &Path) -> Result<()>   // L802
/// The destination path for copying `src` *into* directory `dir`.
pub fn dest_in(dir: &Path, src: &Path) -> Result<PathBuf>   // L845
```

Private functions carrying the platform code:

```rust
const FICLONE: libc::c_ulong = 0x4004_9409;   // L54
fn sync_file(file: &File, path: &Path) -> Result<()>   // L432  (File::sync_all)
fn temp_beside(dst: &Path) -> Result<PathBuf>   // L520  (".df-tmp-{pid}-{n}" in dst's directory)
fn write_contents(reader: &mut File, writer: &mut File, src: &Path, dst: &Path, ctx: &TaskCtx, len: u64) -> Result<u64>   // L538  (tries reflink, then 1 MiB chunks)
fn reflink(reader: &File, writer: &File) -> bool   // L581  (ioctl FICLONE)
#[cfg(not(test))] fn reflink_enabled() -> bool   // L603
fn set_times(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()>   // L651  (utimensat, AT_SYMLINK_NOFOLLOW, UTIME_OMIT)
fn in_the_way(e: &std::io::Error) -> bool   // L764  (errno ENOTEMPTY/EEXIST/EISDIR/ENOTDIR)
```

Facts: the atime and mtime copies do not follow symlinks (`AT_SYMLINK_NOFOLLOW`). An overwrite, or any durable copy, writes to a temp name beside the destination and then `rename`s it into place (341–386). `move_path` tries `rename` first. On `EXDEV` it falls back to `move_cross_device`, which is `copy_tree(…, true)`, then `verify_copy`, then `remove_tree`.

### du::fstype

```rust
// src/du/fstype.rs   (du/mod.rs:71 re-exports is_remote, magic_of, REMOTE_FS_MAGIC)
/// `f_type` values that mean "this is not local storage".
pub const REMOTE_FS_MAGIC: &[i64] = &[ 0x6969, 0xFF53_4D42, 0xFE53_4D42, 0x517B, 0x6573_5546, 0x5346_414F, 0x6B6C, 0x00C3_6400, 0x0102_1997, 0x7461_636F, 0x4711, 0x0BD0_0BD0 ];   // L33
/// Whether `path` sits on a filesystem an automatic walk should leave alone.
pub fn is_remote(path: &Path) -> bool   // L53
/// The `f_type` of the filesystem `path` is on.
#[allow(unsafe_code)]
pub fn magic_of(path: &Path) -> Option<i64>   // L62
```

Facts: `is_remote` returns `false` whenever `statfs` fails (61–72). It gates only the automatic walks (app:app.rs:poll_folders, begin_folder_sizes).

### fs::owner

```rust
// src/fs/owner.rs   (pub mod owner; at fs/mod.rs:55)
/// Both files are `name:x:id:…`, colon-separated, one record per line. Lines
pub(crate) fn parse_id_table(text: &str) -> HashMap<u32, String>   // L39
/// The user's name, or `None` if this uid is not in `/etc/passwd`.
pub fn user_name(uid: u32) -> Option<&'static str>   // L65
/// The group's name, or `None` if this gid is not in `/etc/group`.
pub fn group_name(gid: u32) -> Option<&'static str>   // L70
/// What the owner linemode draws: `brian brian`, falling back to the number for
pub fn owner_label(uid: u32, gid: u32) -> String   // L76
// private: fn users() -> &'static HashMap<u32, String> (L25, OnceLock over "/etc/passwd"); fn groups() -> &'static HashMap<u32, String> (L31, "/etc/group"); fn parse_id_file(path: &str) -> HashMap<u32, String> (L54)
```

### fs::entry (fields filled from `Metadata`)

```rust
// src/fs/entry.rs   (fs/mod.rs:67 re-exports Entry, Kind, LinkTarget)
/// What a name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink {
        target: Option<LinkTarget>,
    },
}   // L30
/// What a symlink resolved to. Deliberately not `Kind` — a link to a link
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
    File,
    Dir,
    Other,
}   // L44
/// One directory entry, fully stat'd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub len: u64,
    pub mtime: Option<SystemTime>,
    pub btime: Option<SystemTime>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub is_hidden: bool,
    pub mime: &'static str,
    pub file_kind: super::FileKind,
}   // L55
impl Entry {
    /// Read one entry, given its path. The name is the path's last component.
    pub fn read(path: impl Into<PathBuf>) -> Result<Entry>   // L102
    /// Build from a name, a path and the result of an `lstat`, following the
    pub(crate) fn from_parts(name: String, path: PathBuf, link_meta: std::fs::Metadata) -> Entry   // L118
    /// Whether `→` enters this: a directory, or a symlink to one.
    pub fn is_dir(&self) -> bool   // L183
    pub fn is_symlink(&self) -> bool   // L193
    /// A symlink whose target is gone. Drawn in red, never previewed, and still
    pub fn is_broken_symlink(&self) -> bool   // L199
    /// Where a symlink points, unresolved (the literal target path, which may
    pub fn link_target(&self) -> Option<PathBuf>   // L206
    /// The extension, lowercased, empty when there isn't one.
    pub fn extension(&self) -> &str   // L218
    /// The name without its extension — what `R` renames and what `r` selects.
    pub fn stem(&self) -> &str   // L227
    /// `drwxr-xr-x`, for the permissions linemode and the spot panel.
    pub fn permissions_string(&self) -> String   // L236
    /// The owner as the linemode draws it.
    pub fn owner_label(&self) -> String   // L273
    /// The parent directory, or `None` at the root.
    pub fn parent(&self) -> Option<&Path>   // L278
}
```

How the fields are filled in `from_parts` (118–180). The `link_meta` comes from `symlink_metadata`, or from `DirEntry::metadata` in fs/scan.rs:327/368. For a symlink, `std::fs::metadata(&path)` is followed, and the target's metadata is used when it resolves.

| Field | Source |
|---|---|
| `name` | Caller (`file_name().to_string_lossy()`: fs/scan.rs:328/369, entry.rs:104–109) |
| `kind` | `link_meta.file_type().is_symlink()`, then followed `metadata()`: `is_dir()`/`is_file()`/other |
| `len` | `meta.len()`, and 0 for directories |
| `mtime` | `meta.modified().ok()` |
| `btime` | `meta.created().ok()` |
| `mode` | `MetadataExt::mode()` (raw `st_mode`, type and permission bits) |
| `uid` / `gid` | `MetadataExt::uid()` / `gid()` |
| `is_hidden` | `name.starts_with('.')` |
| `mime` | `mime::DIR_MIME` / `BROKEN_LINK_MIME` / `mime::hint_for_name(&name)` |
| `file_kind` | `kind::classify(kind, &name, mime, meta.mode())` |

Other `Entry` constructors that fill these fields by hand: core:vfs/mod.rs:remote_entry (1172–1196; mode/uid/gid from SFTP attrs), app:trashview.rs:row_from (213–244), and tests.

### vfs::poll

```rust
// src/vfs/poll.rs   (private module: `mod poll;` at vfs/mod.rs:63)
/// Time left until `deadline`, or `None` if it has passed.
pub fn remaining(deadline: Instant) -> Option<Duration>   // L55
/// Wait until either descriptor has something to read, or the timeout expires.
pub fn poll_read2(stdout: i32, stderr: i32, timeout: Duration) -> io::Result<(bool, bool)>   // L70
/// Wait until a descriptor will accept a write, or the timeout expires.
pub fn poll_write(fd: i32, timeout: Duration) -> io::Result<bool>   // L105
/// Put a descriptor in non-blocking mode.
pub fn set_nonblocking(fd: i32) -> io::Result<()>   // L132
// private: fn millis(timeout: Duration) -> i32 (L45, rounds up to ≥1 ms)
```

Facts: the file owns no descriptors. It borrows raw fds from `ChildStdin`, `ChildStdout` and `ChildStderr` via `AsRawFd` in vfs/conn.rs. `EINTR` is reported as "nothing ready".

### vfs::conn (how it spawns ssh and reads pipes)

```rust
// src/vfs/conn.rs   (private module: `mod conn;` at vfs/mod.rs:62; vfs/mod.rs:74 re-exports the four consts)
/// Bytes per `READ`/`WRITE`.
pub const READ_CHUNK: u32 = 32 * 1024;   // L74
/// How many `READ`s are outstanding at once.
pub const READ_WINDOW: usize = 16;   // L84
/// How many `WRITE`s are outstanding at once. Same arithmetic as
pub const WRITE_WINDOW: usize = 16;   // L89
/// How many `STAT`s a listing will spend resolving symlinks.
pub const MAX_LINK_RESOLVES: usize = 256;   // L105
/// The most of `ssh`'s stderr kept for the error message.
pub const MAX_STDERR: usize = 8 * 1024;   // L112
/// A live SFTP session on one child process.
pub(super) struct Connection {
    service: Arc<Service>,
    transport: Transport,
    next_id: u32,
    home: Option<String>,
}   // L387
impl Connection {
    /// Spawn the child and complete the `INIT`/`VERSION` handshake.
    pub(super) fn connect(service: Arc<Service>) -> Result<Connection, VfsError>   // L404
    /// Resolve a path server-side. Returns the canonical absolute path.
    pub(super) fn realpath(&mut self, path: &VfsPath) -> Result<String, VfsError>   // L538
    /// `STAT` (follows symlinks) or `LSTAT` (does not).
    pub(super) fn stat(&mut self, path: &VfsPath, follow: bool) -> Result<Attrs, VfsError>   // L602
    pub(super) fn mkdir(&mut self, path: &VfsPath) -> Result<(), VfsError>   // L613
    pub(super) fn rmdir(&mut self, path: &VfsPath) -> Result<(), VfsError>   // L625
    pub(super) fn remove(&mut self, path: &VfsPath) -> Result<(), VfsError>   // L631
    pub(super) fn rename(&mut self, from: &VfsPath, to: &VfsPath) -> Result<(), VfsError>   // L637
    pub(super) fn readlink(&mut self, path: &VfsPath) -> Result<String, VfsError>   // L644
    /// Create a symlink. `target` is what it points at, `link` is what is
    pub(super) fn symlink(&mut self, target: &str, link: &VfsPath) -> Result<(), VfsError>   // L666
    /// `chmod`, as the `SETSTAT` it is.
    pub(super) fn chmod(&mut self, path: &VfsPath, mode: u32) -> Result<(), VfsError>   // L679
    pub(super) fn opendir(&mut self, path: &VfsPath) -> Result<Vec<u8>, VfsError>   // L695
    /// One `READDIR`. `Ok(None)` is the end of the directory.
    pub(super) fn readdir(
        &mut self,
        path: &VfsPath,
        handle: &[u8],
    ) -> Result<Option<Vec<wire::NameEntry>>, VfsError>   // L702
    /// Close a handle, and say so in the log rather than to the user when it
    pub(super) fn close_quietly(&mut self, handle: &[u8])   // L725
    /// Follow a batch of symlinks with pipelined `STAT`s.
    pub(super) fn stat_many(&mut self, paths: &[VfsPath]) -> Result<Vec<Option<Attrs>>, VfsError>   // L738
    /// Download `remote` to `local`, pipelined. Returns the byte count.
    pub(super) fn download(
        &mut self,
        remote: &VfsPath,
        local: &Path,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError>   // L773
    /// Upload `local` to `remote`, pipelined. Returns the byte count.
    pub(super) fn upload(
        &mut self,
        local: &Path,
        remote: &VfsPath,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError>   // L949
}
/// Turn `ssh`'s parting words into the right error.
pub(super) fn classify_ssh_failure(service: String, detail: String) -> VfsError   // L1120
```

The private transport that holds the platform code:

```rust
struct Transport { service: String, child: Child, stdin: ChildStdin, stdout: ChildStdout, stderr: ChildStderr, inbuf: Vec<u8>, scratch: Vec<u8>, errbuf: Vec<u8>, stderr_done: bool }   // L138
fn spawn(service: &Service) -> Result<Transport, VfsError>   // L155
fn slurp_stderr(&mut self)   // L236
fn drain_stderr(&mut self)   // L256  (STDERR_GRACE = 200 ms)
fn fill(&mut self, want: usize, deadline: Instant, op: &'static str) -> Result<(), VfsError>   // L271
fn read_packet(&mut self, deadline: Instant, op: &'static str) -> Result<Packet, VfsError>   // L311
fn write_all(&mut self, mut bytes: &[u8], deadline: Instant, op: &'static str) -> Result<(), VfsError>   // L337
impl Drop for Transport { fn drop(&mut self) }   // L378  (child.kill(); child.wait())
```

How it spawns and reads:

1. `Transport::spawn` calls `service.command()` (vfs/config.rs:181). That builds `ssh -x -o BatchMode=yes -o ConnectTimeout=15 [-p N] [-i key] -s <user@host> sftp`, or `Service.program` with its args.
2. It sets all three stdio streams to `Stdio::piped()`, spawns, and `take()`s the three handles.
3. `stdin` and `stderr` are switched to `O_NONBLOCK` (`poll::set_nonblocking`, 184–188). `stdout` stays blocking.
4. Every read is `poll_read2(stdout_fd, stderr_fd | -1, remaining)`, followed by `stdout.read`. Every write is `poll_write(stdin_fd, remaining)`, followed by `stdin.write` (partial writes allowed).
5. Deadlines are `CONNECT_TIMEOUT` (15 s, vfs/mod.rs:97) for the handshake and `OP_TIMEOUT` (30 s, vfs/mod.rs:107) after it. When one expires, the result is `VfsError::Timeout`.
6. EOF on stdout triggers `drain_stderr` for ≤200 ms, then `Disconnected` carrying ssh's stderr text. `Drop` kills and reaps the child.
7. Local files: `download` uses `File::create(local)`, `write_all_at(data, offset)` (897) and `sync_all` (803). `upload` uses `File::open(local)` and sequential `read`.

### thread (lower_priority)

```rust
// src/thread.rs
/// Lower the **calling thread**'s scheduling priority to `nice` (1–19; higher
#[allow(unsafe_code)]
pub fn lower_priority(nice: i32) -> bool   // L27
/// How nice a **bulk** worker should be: the copy/move/delete pool, twenty
pub const NICE_BULK: i32 = 10;   // L48
/// How nice an **interactive** worker should be: preview, decode, document
pub const NICE_INTERACTIVE: i32 = 3;   // L53
```

Facts: the body is `libc::setpriority(libc::PRIO_PROCESS, 0, nice.clamp(0, 19))`, and 0 returns `false` without a syscall. There are 10 call sites, all from inside spawned threads: 3 in df-core and 7 in df-app (listed in §1 thread.rs).

### state (where it puts files; how it finds HOME/XDG)

```rust
// src/state/mod.rs
pub mod pins;   // L82
pub use pins::{Pin, PinRefusal};   // L83
/// How many directories are remembered.
pub const MAX_STATE_ENTRIES: usize = 2000;   // L101
/// The largest state file that will be read, in bytes.
pub const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;   // L109
/// The first line of a state file.
pub const HEADER: &str = "# delightfile state v1";   // L112
/// The key of the one non-directory record. Not a valid absolute path, which is
pub const TABS_KEY: &str = "!tabs";   // L116
/// How a tab draws its directory (PLAN §2): the question
pub enum View {
    List,
    Grid,
}   // L125
impl View {
    pub fn from_name(name: &str) -> Option<View>   // L134
    pub fn name(self) -> &'static str   // L142
    /// The other one. What the toggle key does.
    pub fn toggled(self) -> View   // L150
}
/// A sort chosen for one directory.
pub struct SortOverride {
    pub by: SortBy,
    pub reverse: bool,
}   // L164
/// Everything remembered about one directory. Every field is optional and
pub struct ViewState {
    pub sort: Option<SortOverride>,
    pub linemode: Option<LineMode>,
    pub show_hidden: Option<bool>,
}   // L174
impl ViewState {
    /// Whether anything is set. An empty state is not stored — see
    pub fn is_empty(&self) -> bool   // L183
}
/// The store: load it at startup, read and write it as the user moves around,
pub struct StateStore {
    path: PathBuf,
    dirs: HashMap<PathBuf, Record>,
    tabs: Vec<PathBuf>,
    active_tab: usize,
    tabs_touched: u64,
    pins: Vec<Pin>,
    dirty: bool,
}   // L200
impl StateStore {
    /// Where the state file lives: `$XDG_STATE_HOME/delightfile/state`, falling
    pub fn state_path() -> Option<PathBuf>   // L221
    /// Load from the default location. Never fails: a missing file is a fresh
    pub fn load() -> StateStore   // L228
    /// Load from an explicit path. For tests, and for a `--state` flag if one
    pub fn load_from(path: impl Into<PathBuf>) -> StateStore   // L242
    /// Where this store saves, empty when it has nowhere to.
    pub fn path(&self) -> &Path   // L280
    /// Whether there are unsaved changes. The app's debounce timer reads this to
    pub fn is_dirty(&self) -> bool   // L286
    pub fn len(&self) -> usize   // L290
    pub fn is_empty(&self) -> bool   // L294
    /// Everything remembered about `dir`, or `None` if it has never been
    pub fn get(&self, dir: &Path) -> Option<ViewState>   // L305
    pub fn sort(&self, dir: &Path) -> Option<SortOverride>   // L309
    pub fn linemode(&self, dir: &Path) -> Option<LineMode>   // L313
    pub fn show_hidden(&self, dir: &Path) -> Option<bool>   // L317
    /// The directories the tabs were last on, oldest tab first. Empty until
    pub fn tabs(&self) -> &[PathBuf]   // L323
    /// Which tab was focused, as an index into [`StateStore::tabs`]. Clamped on
    pub fn active_tab(&self) -> usize   // L329
    /// Set or clear the sort override. `None` clears it; a record left with no
    pub fn set_sort(&mut self, dir: impl Into<PathBuf>, sort: Option<SortOverride>)   // L339
    pub fn set_linemode(&mut self, dir: impl Into<PathBuf>, linemode: Option<LineMode>)   // L343
    pub fn set_hidden(&mut self, dir: impl Into<PathBuf>, show_hidden: Option<bool>)   // L347
    /// Replace everything remembered about one directory at once.
    pub fn set(&mut self, dir: impl Into<PathBuf>, state: ViewState)   // L352
    /// Forget a directory. Returns whether there was anything to forget.
    pub fn clear(&mut self, dir: &Path) -> bool   // L357
    /// Mark a directory as recently used without changing its settings, so the
    pub fn touch(&mut self, dir: &Path)   // L367
    /// Record where the tabs are, for session restore.
    pub fn set_tabs(&mut self, tabs: Vec<PathBuf>, active: usize)   // L375
    /// Write the file, atomically, if anything has changed.
    pub fn flush(&mut self) -> Result<()>   // L431
    /// The file's exact bytes. Public so a test can check the format without
    pub fn render(&self) -> Vec<u8>   // L467
}
/// [`StateStore::state_path`]'s rule, with the environment passed in.
pub fn state_path_from(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf>   // L637
/// Escape the bytes that would otherwise be structure or line noise. See the
pub fn escape(bytes: &[u8]) -> Vec<u8>   // L679
/// Reverse [`escape`]. `None` on a truncated or unknown escape, which is what
pub fn unescape(bytes: &[u8]) -> Option<Vec<u8>>   // L700
/// The spelling [`SortBy::from_name`] accepts, so a file this crate writes is a
pub fn sort_name(sort: SortBy) -> &'static str   // L772
/// The spelling [`LineMode::from_name`] accepts.
pub fn linemode_name(mode: LineMode) -> &'static str   // L786
// private: fn path_bytes(path: &Path) -> &[u8] (L744, OsStrExt); fn path_from(bytes: &[u8]) -> PathBuf (L749, OsStringExt)
```

```rust
// src/state/pins.rs
/// The record key of one pin. See the module header for why it cannot collide
pub const PIN_KEY: &str = "!pin";   // L44
/// One pinned place.
pub struct Pin {
    pub path: String,
    pub key: Option<String>,
}   // L48
impl Pin {
    /// The path with `~` replaced by `$HOME`, by the rule `[goto]` uses.
    pub fn expanded_path(&self) -> String   // L59
    /// Whether this pin is the place `path` names, in whichever of its
    pub fn is(&self, path: &str) -> bool   // L67
}
/// Why the store would not do what it was asked.
pub enum PinRefusal {
    AlreadyPinned(String),
    NotPinned(String),
    KeyTaken { key: String, holder: String },
    NotAKey(String),
}   // L74
impl StateStore {
    /// The pinned places, in the order they were pinned.
    pub fn pins(&self) -> &[Pin]   // L122
    /// The pin for `path`, in any of its spellings.
    pub fn pinned(&self, path: &str) -> Option<&Pin>   // L127
    pub fn is_pinned(&self, path: &str) -> bool   // L131
    /// Put `path` at the end of the list, with `key` or none.
    pub fn pin(&mut self, path: impl Into<String>, key: Option<String>) -> Result<(), PinRefusal>   // L141
    /// Take `path` off the list, and hand back what it was.
    pub fn unpin(&mut self, path: &str) -> Option<Pin>   // L153
    /// Give a pin a different key, or take its key away. The pin keeps its
    pub fn set_pin_key(&mut self, path: &str, key: Option<String>) -> Result<(), PinRefusal>   // L161
}
```

Where files go:

- **Path.** `$XDG_STATE_HOME/delightfile/state` when that variable is non-empty. Otherwise `$HOME/.local/state/delightfile/state`. Otherwise no path, and the store is session-only (`load` 228–238 logs a warning). Relative `XDG_STATE_HOME` values are accepted as-is (637–645).
- **Save.** `flush` runs `create_dir_all(parent)`, writes the temp file `<dir>/.state.tmp.<pid>.<nanos>`, then `rename`s it over the target (439–459).
- **Environment.** `HOME` and `XDG_STATE_HOME` are read only through `state_path()`. df-app also calls `state_path_from` directly (app:portal/request.rs:from_env).

### config (where it looks; Unix-only defaults)

Relevant items, verbatim. The rest of config.rs's pub API is platform-neutral parsing and is not repeated here: `DEFAULT_RATIO`, `DEFAULT_SCROLLOFF`, `DEFAULT_FOLDER_SIZE_TTL`, `DEFAULT_MICRO_WORKERS`, `DEFAULT_MACRO_WORKERS`, `DEFAULT_BIZARRE_RETRY`, `DEFAULT_TAB_SIZE`, `DEFAULT_IMAGE_QUALITY`, `SortBy`, `LineMode`, `ViewScale`, `VIEW_SCALES`, `MgrConfig`, `TasksConfig`, `PreviewConfig`, `Matcher`, `OpenRule`, `Config` (fields `mgr`, `tasks`, `preview`, `goto`, `openers`, `rules`), `Color`, `DirIcon`, `FileIcon`, `Theme`, `Glob`, `glob_match`.

```rust
// src/config.rs
/// The `g` chord's bookmarks, in which-key order: key, path, description.
pub const DEFAULT_BOOKMARKS: &[(&str, &str, &str)] = &[ … ];   // L155  (entries at L156–168: "~", "~/.config", "~/Downloads", "~/Work", "/mnt/schwabserverroot", "/mnt/schwabserverroot/plex", "/mnt/schwabserverroot/files/Projects", "sftp://showandtour1", "sftp://showandtour2")
/// Openers a rule can name: id, command, blocking, description.
pub const DEFAULT_OPENERS: &[(&str, &str, bool, &str)] = &[ … ];   // L192  (entries L193–281)
/// One `g <key>` destination.
pub struct Bookmark {
    pub key: String,
    pub path: String,
    pub description: String,
}   // L745
impl Bookmark {
    /// The path with a leading `~` replaced by `$HOME`. Left alone when there
    pub fn expanded_path(&self) -> String   // L757
}
/// A place as a person writes one — `~/Work`, `/mnt/x`, `sftp://host/srv` —
pub fn expand_home(path: &str) -> String   // L767
/// The shipped bookmark table, as [`Bookmark`]s.
pub fn default_bookmarks() -> Vec<Bookmark>   // L778
/// A named way to open a file (PLAN §6).
pub struct Opener {
    pub name: String,
    pub command: String,
    pub block: bool,
    pub description: String,
}   // L791
impl Opener {
    /// The job name when this is a built-in rather than a shell command.
    pub fn builtin(&self) -> Option<&str>   // L804
}
impl Config {
    pub fn opener(&self, name: &str) -> Option<&Opener>   // L871
    /// The openers for a file, first rule wins, in picker order (PLAN §6).
    pub fn openers_for(&self, name: &str, mime: &str, is_dir: bool) -> Vec<&Opener>   // L882
    /// Read `delightfile.toml` over the defaults.
    pub fn parse(text: &str, file: &Path) -> (Config, Vec<ConfigWarning>)   // L901
}
impl Theme {
    /// The icon for a directory, first rule wins. `path` is the full path and
    pub fn dir_icon(&self, path: &str, name: &str) -> Option<&DirIcon>   // L1220
}
/// Everything the config directory said, plus everything it got wrong.
pub struct Loaded {
    pub config: Config,
    pub theme: Theme,
    pub warnings: Vec<ConfigWarning>,
}   // L1394
/// `$XDG_CONFIG_HOME/delightfile`, else `~/.config/delightfile` (PLAN §3).
pub fn config_dir() -> Option<PathBuf>   // L1401
/// Load from the user's config directory, or the shipped defaults if there
pub fn load() -> Loaded   // L1411
/// Load from a specific directory. **A missing file is silence** (PLAN §3):
pub fn load_from_dir(dir: &Path) -> Loaded   // L1420
```

Facts:

- **Config lookup.** `config_dir()` is `$XDG_CONFIG_HOME/delightfile` (non-empty) or `$HOME/.config/delightfile`. `load_from_dir` reads `delightfile.toml` and `theme.toml` there. df-app also reads `keymap.toml` from the same directory via `Keymap::apply_overrides_from_dir` (keymap/mod.rs:673; app:app.rs:new at 1821). `vfs.toml` is read from `$XDG_CONFIG_HOME|$HOME/.config/yazi/vfs.toml` and then `config_dir()/vfs.toml` (vfs/config.rs:292–308).
- **Unix-only defaults in `DEFAULT_OPENERS`:**
  - `edit`: `setsid uwsm-app -- "${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@" >/dev/null 2>&1`
  - `zed`: `setsid uwsm-app -- zeditor "$@" …`
  - `zed-workspace`: `… zeditor "$1" …`
  - `terminal-here`: `setsid uwsm-app -- "${TERMINAL:-ghostty}" --working-directory="$1" …`
  - `terminal-at`: `… --working-directory="$(dirname "$1")" …`
  - `open-in-chrome`: `… google-chrome-stable "$@" …`
  - `delightviewer`: `… delightviewer --autoplay "$1" …`
  - `delightviewer-edit`: `… delightviewer --edit "$1" …`
  - `edit-image`: `… pinta "$@" …`
  - `set-wallpaper`: `system-cmd-wallpaper-set "$1"`
  - `optimize-avif`: `system-cmd-image-optimize-yazi "$@"`
  - `bulk-rename`: `zeditor --new --wait "$@"` (blocking)
  - `open`: `xdg-open "$1"`
  - `play`: `mpv --force-window "$@"`
  - `extract`, `extract-here`, `extract-merged`: `builtin:*`, not shell
- **Who runs the openers.** df-core never spawns these strings. `Opener.command` is copied by app:open.rs:from (173) and executed in df-app.

### zoxide (data dir lookup)

```rust
// src/zoxide/mod.rs
/// The only database version this parser understands.
pub const DB_VERSION: u32 = 3;   // L52
/// The largest `db.zo` this will read, in bytes.
pub const MAX_DB_BYTES: u64 = 8 * 1024 * 1024;   // L60
/// One directory zoxide remembers.
pub struct ZoxideDir {
    pub path: PathBuf,
    pub rank: f64,
    pub last_accessed: u64,
}   // L71
impl ZoxideDir {
    /// Frecency at time `now`, in seconds since the epoch.
    pub fn score(&self, now: u64) -> f64   // L82
}
/// zoxide's frecency: rank, multiplied or divided by how stale the last visit
pub fn score(rank: f64, last_accessed: u64, now: u64) -> f64   // L99
/// Where zoxide keeps its database: `$XDG_DATA_HOME/zoxide/db.zo`, falling back
pub fn db_path() -> Option<PathBuf>   // L117
/// Every directory zoxide knows about, in the order the file stores them.
pub fn load() -> Vec<ZoxideDir>   // L132
/// [`load`] against a named file, so tests can point at a fixture and so a
pub fn load_from(path: &Path) -> Vec<ZoxideDir>   // L142
/// How well a directory matched a query, so the overlay can rank matches above
pub enum MatchKind {
    Keywords,
    Component,
    Prefix,
}   // L280
/// One row of the `Z` overlay.
pub struct Match {
    pub dir: ZoxideDir,
    pub score: f64,
    pub kind: MatchKind,
}   // L293
/// The `Z` overlay's list: everything matching `query`, best first.
pub fn query(dirs: &[ZoxideDir], query: &str, now: u64) -> Vec<Match>   // L314
```

Facts: `db_path` is `$_ZO_DATA_DIR/db.zo`. Otherwise it is `$XDG_DATA_HOME/zoxide/db.zo` (non-empty), otherwise `$HOME/.local/share/zoxide/db.zo`, otherwise `None` (117–126). No process is spawned.

### preview::cache (thumbnail cache root `$TMPDIR/yazi-$UID`)

```rust
// src/preview/cache.rs
/// The `skip` for a still image — the whole of what a file manager wants.
pub const STILL_SKIP: usize = 0;   // L81
/// The shared cache directory, if it exists.
pub fn cache_dir() -> Option<PathBuf>   // L128
/// The cache file name yazi would write for `path` at `skip`, given `path`'s
pub fn cache_key(path: &Path, meta: &std::fs::Metadata, skip: usize) -> String   // L136
/// Where a thumbnail for `path` lives, whether or not it has been written.
pub fn thumb_path(path: &Path, skip: usize) -> Option<PathBuf>   // L166
/// The cached thumbnail for `path`, if one exists and is current.
pub fn cached_thumb(path: &Path) -> Option<PathBuf>   // L178
/// Where delightfile should *write* a thumbnail for `path`, creating the
pub fn store_thumb(path: &Path, skip: usize) -> Option<PathBuf>   // L195
// private: const CACHE_DIR_PREFIX: &str = "yazi-" (L72); struct KeyHasher (L97); fn uid() -> u32 (L206, libc::getuid)
```

Facts:

- **Root.** `std::env::temp_dir().join(format!("yazi-{}", uid()))`. `cache_dir` returns `None` if the directory does not exist, and `store_thumb` creates it.
- **Key.** XXH3-128 over `0isize`, then `Path::hash`, `len`, `created()`, `ctime` (from `MetadataExt::ctime`/`ctime_nsec`), `modified()` and `skip`. It is formatted with `{:x}` and no zero padding.

### ops::journal and ops::link

```rust
// src/ops/journal.rs   (ops.rs:54 re-exports undo_attempt, undo_record, CopyManifest, CreatedLink, FileKind, Fingerprint, Journal, MovedPath, OpRecord, UndoAttempt, UndoReport, JOURNAL_DEPTH, MAX_MANIFEST_ENTRIES)
/// How many operations `u` can walk back.
pub const JOURNAL_DEPTH: usize = 64;   // L45
/// What kind of thing a path was when the operation finished.
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}   // L49
/// Enough of a path's state to notice that the world moved on.
pub struct Fingerprint {
    pub kind: FileKind,
    pub len: u64,
    pub mtime: Option<SystemTime>,
    pub entries: Option<u64>,
}   // L63
impl Fingerprint {
    pub fn of(path: &Path) -> Result<Fingerprint>   // L75
    /// Is `path` still what it was? The error text says what changed, because
    pub fn verify(&self, path: &Path) -> Result<()>   // L105
}
/// How many paths one copy may record before it stops being undoable.
pub const MAX_MANIFEST_ENTRIES: usize = 50_000;   // L158
/// Every path one copy created, and the state each was in when it finished.
pub struct CopyManifest {
    pub root: PathBuf,
    entries: Vec<(PathBuf, Fingerprint)>,
}   // L178
impl CopyManifest {
    /// Record everything that now lives at `root`.
    pub fn of_tree(root: &Path) -> Result<CopyManifest>   // L200
    /// [`of_tree`](CopyManifest::of_tree) with the bound spelled out, so the
    pub(crate) fn of_tree_capped(root: &Path, cap: usize) -> Result<CopyManifest>   // L206
    /// How many paths the copy created.
    pub fn len(&self) -> usize   // L214
    pub fn is_empty(&self) -> bool   // L218
    /// Every path this manifest covers, absolute.
    pub fn paths(&self) -> impl Iterator<Item = PathBuf> + '_   // L223
    /// Is the world still exactly as the copy left it?
    pub fn verify(&self) -> Result<()>   // L247
    /// Delete exactly the manifested paths, children before parents.
    pub fn remove(&self, ctx: &TaskCtx) -> Result<()>   // L283
    /// What is left of this manifest after a [`remove`](CopyManifest::remove)
    pub fn remaining(&self) -> Option<CopyManifest>   // L311
}
/// One leg of a move, with the fingerprint of where it landed.
pub struct MovedPath {
    pub from: PathBuf,
    pub to: PathBuf,
    pub fingerprint: Fingerprint,
}   // L368
impl MovedPath {
    pub fn record(from: &Path, to: &Path) -> Result<MovedPath>   // L375
}
/// One link a link operation created, and what its inverse needs.
pub struct CreatedLink {
    pub link: PathBuf,
    pub target: Option<PathBuf>,
    pub fingerprint: Fingerprint,
}   // L389
impl CreatedLink {
    /// Fingerprint a link that has just been made. `target` is the text
    pub fn record(link: &Path, target: Option<&Path>) -> Result<CreatedLink>   // L399
}
/// A completed operation, and everything its inverse needs.
pub enum OpRecord {
    Move { moves: Vec<MovedPath> },
    Rename { moved: MovedPath },
    Renames { moved: Vec<MovedPath> },
    Copy { created: Vec<CopyManifest> },
    Trash { items: Vec<TrashedItem> },
    Create {
        path: PathBuf,
        is_dir: bool,
        fingerprint: Fingerprint,
        created_parents: Vec<PathBuf>,
    },
    Link {
        link: PathBuf,
        target: Option<PathBuf>,
        fingerprint: Fingerprint,
    },
    Links { links: Vec<CreatedLink> },
}   // L410
impl OpRecord {
    /// One line for the `u` toast, in the past tense of what would happen.
    pub fn describe(&self) -> String   // L473
}
/// What an undo did, for the toast (PLAN §5: every op lands with an 8 s undo
pub struct UndoReport {
    pub description: String,
    pub touched: Vec<PathBuf>,
}   // L523
/// The bounded stack of inverses.
pub struct Journal {
    entries: VecDeque<OpRecord>,
    depth: usize,
}   // L532
impl Journal {
    pub fn new(depth: usize) -> Journal   // L544
    /// Push a completed operation. The oldest entry falls off the bottom once
    pub fn record(&mut self, record: OpRecord)   // L555
    pub fn len(&self) -> usize   // L562
    pub fn is_empty(&self) -> bool   // L566
    pub fn depth(&self) -> usize   // L570
    /// What `u` would take back next, without taking it back.
    pub fn peek(&self) -> Option<&OpRecord>   // L575
    pub fn clear(&mut self)   // L579
    /// Undo the most recent operation.
    pub fn undo(&mut self, ctx: &TaskCtx) -> Result<UndoReport>   // L591
}
/// The result of one inverse, plus what is left of it if it stopped part way.
pub struct UndoAttempt {
    pub result: Result<UndoReport>,
    pub remaining: Option<OpRecord>,
}   // L615
/// Run one inverse. Public so a caller can undo a record it is holding without
pub fn undo_record(record: &OpRecord, ctx: &TaskCtx) -> Result<UndoReport>   // L629
/// [`undo_record`], keeping what is left over when it fails half way.
pub fn undo_attempt(record: &OpRecord, ctx: &TaskCtx) -> UndoAttempt   // L634
```

```rust
// src/ops/link.rs   (ops.rs:58 re-exports hardlink, relative_to, symlink, LinkKind)
/// Which flavour of link is being made.
pub enum LinkKind {
    Absolute,
    Relative,
}   // L26
/// The relative path from directory `from_dir` to `to`.
pub fn relative_to(from_dir: &Path, to: &Path) -> PathBuf   // L40
/// Create a symbolic link at `link` pointing at `target`.
pub fn symlink(target: &Path, link: &Path, kind: LinkKind) -> Result<PathBuf>   // L78
/// Create a hard link at `link` to the file `target`.
pub fn hardlink(target: &Path, link: &Path) -> Result<()>   // L101
```

Facts:

- **Journal.** It is a `VecDeque<OpRecord>` in memory. There is no file format, and nothing writes or reads it from disk.
- **Undo of a link.** Undo compares `std::fs::read_link(link)` to the recorded target text (871, 911), then calls `remove_file` (879, 933).
- **`ops::link::symlink`.** It writes the text from `normalize(target)` (Absolute) or `relative_to(link.parent(), target)` (Relative) with `std::os::unix::fs::symlink`, and returns that text. It does not pick between file and directory symlinks.
- **`ops::link::hardlink`.** It uses `std::fs::hard_link`.

### archive::external (binaries, spawning, kill/signal)

```rust
// src/archive/external.rs   (archive/mod.rs:89 re-exports external_extractor, Extractor, ExtractorKind)
/// Which program an [`Extractor`] runs.
pub enum ExtractorKind {
    SevenZip,
    Bsdtar,
}   // L42
/// A program on this machine that can extract what this crate cannot.
pub struct Extractor {
    pub kind: ExtractorKind,
    pub program: PathBuf,
}   // L52
/// The extractor to use, in order of preference: 7-Zip, then `bsdtar`.
pub fn external_extractor() -> Option<Extractor>   // L62
/// [`external_extractor`] over a given `PATH`, so the preference order is
pub fn extractor_on(path: &OsStr) -> Option<Extractor>   // L68
pub(super) fn is_executable(path: &Path) -> bool   // L82
impl Extractor {
    /// What to call it in a message.
    pub fn label(&self) -> &'static str   // L89
    /// Whether it can read an archive split across several files. Only 7-Zip:
    pub fn reads_volumes(&self) -> bool   // L98
    /// The arguments that extract `archive` into `dest`, which must exist.
    pub fn args(&self, archive: &Path, dest: &Path, overwrite: bool) -> Vec<OsString>   // L114
    /// The arguments that join a byte-split set (`name.tar.gz.001`, …) back
    pub fn join_args(&self, head: &Path, into: &Path) -> Option<Vec<OsString>>   // L147
}
/// What running an extractor came to.
pub struct Ran {
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}   // L177
impl Ran {
    pub fn succeeded(&self) -> bool   // L186
}
/// Run `program` with `args` to completion, or until the task is cancelled —
pub fn run(program: &Path, args: &[OsString], ctx: &TaskCtx) -> Result<Ran>   // L199
/// [`run`], started in `dir` — for a program whose arguments are names
pub fn run_in(program: &Path, args: &[OsString], dir: Option<&Path>, ctx: &TaskCtx) -> Result<Ran>   // L206
/// Read a pipe to its end on a thread of its own, keeping the first
pub(super) fn drain<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>>   // L256
/// The one line worth showing from a failed extractor.
pub fn failure_line(ran: &Ran, archive: &Path) -> String   // L272
```

Facts:

- **Binaries.** `7z` is preferred over `bsdtar`. Each is found by joining the bare name to each `PATH` entry and checking `is_executable` (mode `& 0o111`).
- **Arguments.**
  - 7z: `x -y -aoa|-aou -o<dest> -- <archive>`. Split sets: `x -tsplit -y -o<into> -- <head>`.
  - bsdtar: `-x -f <archive> -C <dest> [-k]`.
  - archive/write/mod.rs:768 also runs `7z a -t7z -bd -y -snl -- <temp> <names…>`, started in the sources' parent directory.
- **Spawning.** `stdin` is null. `stdout` and `stderr` are piped and drained on two threads, with the first 64 KiB of each kept.
- **Cancel.** The caller thread polls `try_wait` every 50 ms. A cancel calls `child.kill()` and then `child.wait()`, and `Ran.status` is `None`. No POSIX signal API is used. Exit status is read with `success()` and `code()`.

### git::status (spawning, exit-status use)

```rust
// src/git/status.rs   (git/mod.rs:66 re-exports parse_porcelain_v2, status_blocking, DirtyCounts, FileStatus, StatusData, StatusError, MAX_STATUS_BYTES, MAX_STATUS_ENTRIES)
/// The most status entries kept from one repository.
pub const MAX_STATUS_ENTRIES: usize = 200_000;   // L55
/// The most bytes read from `git status`'s stdout: 64 MiB.
pub const MAX_STATUS_BYTES: u64 = 64 * 1024 * 1024;   // L63
/// What one path's git state is, reduced to the one thing a row can show.
pub enum FileStatus {
    Ignored,
    Untracked,
    Added,
    Deleted,
    Renamed,
    Typechange,
    Modified,
    Conflict,
}   // L72
impl FileStatus {
    /// Which status wins when a directory contains several.
    pub fn rank(self) -> u8   // L100
    /// Whether a directory inherits this from its contents. See the module
    pub fn rolls_up(self) -> bool   // L115
    /// The single character for a dot-plus-letter badge, matching what git's own
    pub fn letter(self) -> char   // L121
    /// The worse of two statuses, for the rollup merge.
    pub fn worse(self, other: FileStatus) -> FileStatus   // L135
}
/// The three numbers the breadcrumb shows beside the branch.
pub struct DirtyCounts {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
}   // L150
impl DirtyCounts {
    /// Whether there is anything at all to report. Ignored files do not count —
    pub fn is_clean(&self) -> bool   // L164
    pub fn total(&self) -> usize   // L168
}
/// One repository's status, keyed by absolute path.
pub struct StatusData {
    pub files: HashMap<PathBuf, FileStatus>,
    pub dirs: HashMap<PathBuf, FileStatus>,
    pub collapsed: HashMap<PathBuf, FileStatus>,
    pub counts: DirtyCounts,
    pub branch: Option<String>,
    pub ahead_behind: Option<(i64, i64)>,
    pub truncated: bool,
}   // L180
impl StatusData {
    /// What to draw for one row.
    pub fn status_for(&self, path: &Path) -> Option<FileStatus>   // L217
    /// Whether this path is dimmed, directly or by inheritance.
    pub fn is_ignored(&self, path: &Path) -> bool   // L236
    pub fn len(&self) -> usize   // L240
    pub fn is_empty(&self) -> bool   // L244
}
/// Why a status could not be taken.
pub enum StatusError {
    NoGit,
    Failed(String),
    Io(#[from] std::io::Error),
}   // L254
/// Run `git status` in `root` and parse it. Blocks; only [`super::cache::Git`]'s
pub fn status_blocking(root: &Path) -> Result<StatusData, StatusError>   // L278
/// Parse porcelain v2 `-z` bytes into a status map rooted at `root`.
pub fn parse_porcelain_v2(bytes: &[u8], root: &Path) -> StatusData   // L363
// private: fn insert(data: &mut StatusData, root: &Path, raw: &[u8], status: FileStatus, roll_up: bool) (L556, OsStr::from_bytes)
```

Facts:

- **Command.** `git -c core.fsmonitor= -c core.hooksPath=/dev/null -c core.pager=cat -c core.sshCommand= --no-optional-locks status --porcelain=v2 -z --ignored=matching --branch`, run with `current_dir(root)`.
- **Pipes and waiting.** `stdin` is null. `stdout` is read on the calling thread with `take(64 MiB)`, then `stderr` with `take(8192)`, then `child.wait()`. There is no kill and no timeout.
- **Exit status.** `NotFound` at spawn gives `StatusError::NoGit`. A non-success exit with empty stdout gives `Failed(stderr or "exit {status}")`. No signal inspection.
- **Caller.** It runs on the `Git` cache worker thread (core:git/cache.rs:run_one; thread spawned at git/cache.rs:126–128).

### rename::facts (libc use)

```rust
// src/rename/facts.rs
/// A local civil date-time. EXIF stamps carry no zone, so everything here is
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}   // L34
impl Civil {
    /// A `SystemTime` on the machine's local wall clock.
    #[allow(unsafe_code)]
    pub fn local(time: SystemTime) -> Option<Civil>   // L54
    /// The wall clock right now, for `{date}`'s "is this taken date believable"
    pub fn now() -> Option<Civil>   // L92
}
/// What a photo reader found. `Pending` is "a worker is still reading it".
pub enum Photo {
    Pending,
    None,
    Some(PhotoFacts),
}   // L104
/// The four questions the template asks of a photo, answered as far as the file
pub struct PhotoFacts {
    pub taken: Option<Civil>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub make: Option<String>,
    pub model: Option<String>,
}   // L114
/// One row's worth of knowledge: the name, the stat, and (eventually) the
pub struct Facts {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<Civil>,
    pub created: Option<Civil>,
    pub parent: String,
    pub photo: Photo,
}   // L125
impl Facts {
    /// Stat `dir/name`, without following a symlink: a link is renamed as a
    pub fn stat(dir: &Path, name: &str) -> Facts   // L150
    /// The name without its extension.
    pub fn stem(&self) -> &str   // L176
    /// The extension **with** its dot (`.jpg`), or empty. Carrying the dot is
    pub fn ext(&self) -> &str   // L184
}
```

Facts: the libc items used are `libc::time_t` (67), `libc::tm` via `std::mem::zeroed` (70) and `libc::localtime_r(&t, &mut tm)` (74). The fields read are `tm_year`, `tm_mon`, `tm_mday`, `tm_hour`, `tm_min` and `tm_sec`. `Facts::stat` uses std's `symlink_metadata`, `modified()` and `created()` only.

### ops.rs root (libc use)

```rust
// src/ops.rs
pub mod copy;   // L34
pub mod create;   // L35
pub mod delete;   // L36
pub mod jobs;   // L37
pub mod journal;   // L38
pub mod link;   // L39
pub mod paste;   // L40
pub mod trash;   // L41
pub use copy::{copy_tree, copy_tree_with, measure, move_cross_device, move_path, CopyOptions, CopyStats, COPY_CHUNK};   // L47
pub use create::{create, rename, Created};   // L51
pub use delete::{check_deletable, check_deletable_here, delete_permanent, remove_tree};   // L52
pub use jobs::{DeleteJob, ExtractJob, OpOutcome, Outcome, PasteJob, TrashJob};   // L53
pub use journal::{undo_attempt, undo_record, CopyManifest, CreatedLink, FileKind, Fingerprint, Journal, MovedPath, OpRecord, UndoAttempt, UndoReport, JOURNAL_DEPTH, MAX_MANIFEST_ENTRIES};   // L54
pub use link::{hardlink, relative_to, symlink, LinkKind};   // L58
pub use paste::{execute as paste, plan_paste, unique_name, Clipboard, Conflict, PasteItem, PasteMode, PastePlan, PasteReport, Resolution, Toggled};   // L59
pub use trash::{purge, Trash, TrashedItem};   // L63
/// Make a path absolute and lexically clean, without touching the disk.
pub fn normalize(path: &Path) -> PathBuf   // L73
/// Is this a URL — `sftp://host/photos`, `trash://` — rather than a path?
pub fn is_url(path: &Path) -> bool   // L109
/// Is `ancestor` at or above `path` in the tree? Lexical, over normalized
pub fn is_ancestor(ancestor: &Path, path: &Path) -> bool   // L124
/// Strictly below: an ancestor that is not the path itself.
pub fn is_strict_ancestor(ancestor: &Path, path: &Path) -> bool   // L131
/// [`is_ancestor`], asking the filesystem instead of the text.
pub fn is_ancestor_resolved(ancestor: &Path, path: &Path) -> bool   // L158
/// [`is_strict_ancestor`], resolved. See [`is_ancestor_resolved`].
pub fn is_strict_ancestor_resolved(ancestor: &Path, path: &Path) -> bool   // L169
/// Is this a directory *itself*, rather than a symlink to one?
pub fn is_real_dir(path: &Path) -> bool   // L183
/// A path with any trailing `/` trimmed, for the calls that must act on the
pub fn trim_trailing_slash(path: &Path) -> PathBuf   // L196
/// Are these two paths the same file *on disk* (same device and inode)?
pub fn same_file(a: &Path, b: &Path) -> bool   // L212
/// The file name of a path, or an error naming the path that has none
pub fn file_name(path: &Path) -> Result<&std::ffi::OsStr>   // L222
/// Does this path exist, counting a broken symlink as existing?
pub fn exists(path: &Path) -> bool   // L231
#[cfg(test)] pub(crate) use crate::test_support as fixture;   // L241
// private: fn resolved(path: &Path) -> Option<PathBuf> (L142, std::fs::canonicalize)
```

Facts: ops.rs uses no `libc`. Its platform dependencies are `std::os::unix::ffi::OsStrExt` (110, 197) and `std::os::unix::fs::MetadataExt` (213, `dev()`/`ino()`). The `libc` users under `ops/` are ops/copy.rs (`c_ulong`, `ioctl`, `timespec`, `time_t`, `utimensat`, `AT_FDCWD`, `AT_SYMLINK_NOFOLLOW`, `EINVAL`, `EXDEV`, `ENOTEMPTY`, `EEXIST`, `EISDIR`, `ENOTDIR`) and ops/trash.rs (`EXDEV`, `getuid`).

### Supporting: vfs::config spawn surface

```rust
// src/vfs/config.rs   (vfs/mod.rs:73 re-exports config_paths, Service, ServiceKind, VfsConfig, DEFAULT_SSH_PORT)
/// The port `ssh` uses when nothing says otherwise.
pub const DEFAULT_SSH_PORT: u16 = 22;   // L54
/// One remote machine.
pub struct Service {
    pub name: String,
    pub kind: ServiceKind,
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
    pub key_file: Option<String>,
    pub root: Option<String>,
    pub program: Option<(PathBuf, Vec<String>)>,
}   // L75
impl Service {
    /// An sftp service with everything left to `ssh`.
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Service   // L110
    /// A service that runs `program` directly. See [`Service::program`].
    pub fn direct(
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        args: Vec<String>,
    ) -> Service   // L124
    /// `user@host`, or just `host`.
    pub fn destination(&self) -> String   // L136
    /// The key file with a leading `~` expanded, if there is one.
    pub fn key_path(&self) -> Option<PathBuf>   // L144
    /// The path a bare `/` means for this service.
    pub fn root_path(&self) -> &str   // L154
    /// The command that will speak SFTP on its stdio.
    pub fn command(&self) -> std::process::Command   // L181
}
/// Where [`VfsConfig::load`] looks, in precedence order (last wins).
pub fn config_paths() -> Vec<PathBuf>   // L292
```

---

## 3. Path model

### 3.1 Path ↔ bytes conversions (`OsStrExt::as_bytes`, `OsStr::from_bytes`, `OsStringExt::from_vec`/`into_vec`)

All of these are **Unix-only**, because `std::os::windows::ffi` exposes `encode_wide`/`from_wide` (UTF-16) and not bytes. The `str::as_bytes()` calls found by grep are excluded. They take UTF-8 `str`, not `OsStr`, at archive/tree.rs:416, 479; archive/extract.rs:193; archive/write/tar.rs:151–206; config.rs:1597; fs/sort.rs:152; rename/exif.rs:365, 467; state/mod.rs:469–503 (constants); state/pins.rs:200–202 (`String` pin paths); toml.rs:385; vfs/conn.rs:670; vfs/mod.rs:1109; vfs/wire.rs:526.

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| archive/write/mod.rs:536 | Top-level member name `name.as_bytes().to_vec()` | Unix-only | `Pack::run` | Written into zip/tar headers ✓ S1.16 ✓ P3.7 |
| archive/write/mod.rs:562 | Symlink target `as_os_str().as_bytes()` | Unix-only | as above | Stored as the link payload ✓ S1.16 ✓ P3.7 |
| archive/write/mod.rs:580 | Child name `child.as_bytes()` | Unix-only | as above | ✓ S1.16 ✓ P3.7 |
| du/fstype.rs:64 | `CString::new(path.as_os_str().as_bytes())` for `statfs` | Unix-only | `is_remote` | ✓ S1.5 |
| fs/inotify.rs:109 | `CString::new(path.as_os_str().as_bytes())` for `inotify_add_watch` | Linux-only | `add_watch` | ✓ S1.4 |
| git/status.rs:564 | `Path::new(OsStr::from_bytes(trimmed))` from git `-z` output | Unix-only | `parse_porcelain_v2` | ✓ S1.16 ✓ P3.8 |
| ops.rs:111 | `is_url` scans `as_bytes()` | Unix-only | core:ops/paste.rs:carried; core:sync/plan.rs:roots | ✓ S1.16 ✓ P3.10 |
| ops.rs:198–203 | `trim_trailing_slash` bytes in and out (`from_bytes`) | Unix-only | core:ops/delete.rs | ✓ S1.16 ✓ P3.10 |
| ops/copy.rs:672 | `CString::new(path.as_os_str().as_bytes())` for `utimensat` | Unix-only | `set_times` | ✓ S1.5 |
| ops/create.rs:36–43 | `create`: bytes in, trailing `/` check, `from_bytes` out | Unix-only | app:app.rs:create | ✓ S1.16 ✓ P3.10 |
| ops/trash.rs:266–270 | `list`: `file.as_bytes()`, `OsString::from_vec` | Unix-only | app:app.rs:show_trash | ✓ S1.6 |
| ops/trash.rs:613–632 | `fit`: `file_stem().as_bytes()`, `extension().as_bytes()`, `OsString::from_vec` | Unix-only | `suffixed`, `claim_name` | ✓ S1.16 |
| ops/trash.rs:682 | `encode_path`: `as_bytes()` | Unix-only | `trashinfo_text` | ✓ S1.6 |
| ops/trash.rs:719 | `decode_path`: `OsString::from_vec(out)` | Unix-only | `parse_trashinfo` | ✓ S1.6 |
| state/mod.rs:746 | `path_bytes`: `as_bytes()` | Unix-only | `render` | ✓ S1.16 |
| state/mod.rs:751 | `path_from`: `OsString::from_vec` | Unix-only | `parse`, `parse_tabs` | ✓ S1.16 |
| sync/mod.rs:315–316 | `is_debris`: `name.as_bytes().starts_with(TEMP_PREFIX.as_bytes())` | Unix-only | plan helpers | ✓ S1.16 ✓ P3.9 |
| sync/mod.rs:481 | `writable`: `CString::new(dir.as_os_str().as_bytes())` | Unix-only | `trash_available` | ✓ S1.5 |
| sync/rsync.rs:231, 242 | `endpoint`: `as_bytes()` in, `OsStr::from_bytes` out | Unix-only | rsync argv | ✓ S1.16 ✓ P3.9 |
| sync/rsync.rs:398 | `parse_line`: `OsString::from_vec(name)` | Unix-only | `parse_itemized` | ✓ S1.16 ✓ P3.9 |
| sync/rsync.rs:1146 | `remote_digests`: `name.as_os_str().as_bytes()` to stdin | Unix-only | `verify_remote` | ✓ S1.16 ✓ P3.9 |
| sync/rsync.rs:1209 | `parse_sha256sum`: `OsString::from_vec(name)` | Unix-only | `verify_remote` | ✓ S1.16 ✓ P3.9 |

### 3.2 Splitting, joining or trimming a local path on a literal `/`

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| ops.rs:200 | `trim_trailing_slash` trims only `b'/'` | Windows-differs | core:ops/delete.rs | ✓ P3.10 |
| ops/create.rs:37 | Trailing `b'/'` marks "create a directory" | Windows-differs | app:app.rs:create | ✓ P3.10 |
| ops/trash.rs:489 | `home.join(".local/share/Trash")` (embedded `/`) | Linux-only | `Trash::home` | ✓ S1.6 |
| ops/trash.rs:683 | `encode_path` leaves `/` unescaped as the separator | Windows-differs | `trashinfo_text` | ✓ S1.6 |
| state/pins.rs:59, 93 | `expand_home` splices `HOME` + `"/Work"` from `~/Work` | Windows-differs | pins, bookmarks | ✓ P3.14 |
| config.rs:771–772 | `expand_home` string concatenation | Windows-differs | bookmarks, pins | ✓ P3.14 |
| config.rs:1222 | `dir_icon`: pattern `contains('/')` → match against the full path string | Windows-differs | app:icons.rs:icon_for | ✓ P3.15 |
| vfs/config.rs:146–150 | `key_path`: `~` + `HOME` string splice | Windows-differs | `command` | ✓ P3.14 |
| zoxide/mod.rs:123, 125 | `.join(".local/share")`, `.join("zoxide/db.zo")` | Linux-only | `db_path` | ✓ S1.11 |
| zoxide/mod.rs:340 | `path.rsplit('/')` for the last component | Windows-differs | `query` | ✓ P3.16 |
| archive/write/mod.rs:187, 213 | `text.rsplit('/')` for the leaf of a typed archive name | Windows-differs | app:app/compress.rs | ✓ P3.16 |
| git/status.rs:557 | Trailing `b'/'` in git's output means a directory | (git format) | `insert` | git uses `/` on every platform |
| sync/mod.rs:276, 279 | `label` joins with `/` and appends `/` | Windows-differs | app:sync.rs | ✓ P3.17 |
| sync/rsync.rs:232–236 | `endpoint` trims and appends `b'/'` | Unix-only | rsync argv | |
| sync/rsync.rs:391–393 | `parse_line` trailing `/` means folder | (rsync format) | | |
| sync/rsync.rs:1206 | `parse_sha256sum` strips `./` | (sha256sum format) | | |
| preview/cache.rs:130, 197 | `dir.push(format!("yazi-{uid}"))` | Unix-only (uid) | | ✓ S1.8 |
| archive/tree.rs:452–464 | `normalize` splits on both `/` and `\`, rejoins with `/` | already dual | archive listing | Archive-internal names, not local paths |
| vfs/mod.rs:140–202 | `VfsPath::{parse,to_url,join,parent,name}` split and join on `/` | Server side (Unix) | vfs, df-app | Remote paths are `String`s, not `PathBuf`s |
| vfs/conn.rs:493–506 | `wire_path`: `/` root, `starts_with('/')`, `format!("{home}/{raw}")` | Server side | every SFTP request | |

### 3.3 Sites that treat `/` as the root or rely on `Path::new("/")`

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| ops.rs:94–96 | `normalize` returns `"/"` when components pop to empty | Windows-differs | ~30 callers | ✓ P3.12 |
| ops/delete.rs:35 | Root rail `target == Path::new("/")` | Windows-differs | `check_deletable_here` | ✓ P3.13 |
| ops/delete.rs:63 | cwd fallback `PathBuf::from("/")` | Windows-differs | as above | ✓ P3.13 |
| ops/create.rs:145 | Error text falls back to `Path::new("/")` | Windows-differs | `rename` | Display only ✓ P3.13 |
| ops/link.rs:88 | Link parent fallback `PathBuf::from("/")` | Windows-differs | `symlink` | ✓ P3.13 |
| ops/trash.rs:568–582 | `mount_point_of` walks `parent()` to the top by `dev` | Unix-only | trash, sync | The top on Windows is a drive root or UNC share |
| fs/entry.rs:104–109 | A path with no `file_name()` ("`/`") uses the whole path as its name | Windows-differs | `Entry::read` | `C:\` also has no `file_name()` ✓ P3.13 |
| rename/facts.rs:159–164 | A file at the root gets an empty `parent` | (portable) | `Facts::stat` | |
| state/mod.rs:536 | A record key must start with `/` | Windows-differs | `parse` | |
| du/walk.rs (doc 37–41) | Boundary rule described in terms of `/`, `/proc`, `/mnt` | n/a | | Documentation only |
| config.rs:160–166 | Bookmarks under `/mnt/…` | Windows-/macOS-differs | defaults | |

### 3.4 Comparisons that are case-sensitive by construction

`std::path::Path` equality, ordering and hashing compare components exactly, with no case folding on any platform. The rows below compare paths or names that can reach the comparison in different spellings: one from the user, config, a persisted file or another program, the other from the filesystem.

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| ops.rs:124–135 | `is_ancestor` / `is_strict_ancestor`: `normalize` + `starts_with` | Windows-/macOS-differs (case) | delete rails; app:app/syncing.rs:paste_sync | ✓ P3.19 |
| ops.rs:158–177 | `*_resolved`: `canonicalize` both sides, `starts_with` | (canonical spelling on both sides) | paste, copy, sync | ✓ P3.18 |
| ops/delete.rs:40–54 | Home and cwd rails | (case) | `check_deletable` | ✓ P3.19 |
| ops/create.rs:139–141 | Same-directory check for rename | (case) | `rename` | ✓ P3.18 |
| ops/paste.rs:128–176 | `Clipboard` membership (`Vec`/`HashSet<PathBuf>`) | (case) | yank/cut/toggle | ✓ P3.20 |
| ops/paste.rs:193 | `spans_directories`: `parent != first` | (case) | df-app tray | ✓ P3.20 |
| ops/paste.rs:273 | `resolve`: `c.src == src` | (case) | conflict dialog | ✓ P3.18 |
| ops/paste.rs:400, 457, 462 | `claimed.contains` | (case) | `plan_paste`, `unique_name` | ✓ P3.20 |
| ops/trash.rs:316 | `orphans`: `item.name == name` (`OsString`) | (case) | `list` | Both sides come from `read_dir` ✓ P3.20 |
| ops/journal.rs:872, 911 | `read_link(link) != target` | (case) | undo | Link text compare ✓ P3.20 |
| fs/history.rs:54 | `History::push`: `dir == self.current` | (case) | tab history | ✓ P3.19 |
| fs/memory.rs:29, 53, 69 | `Recent`: `HashMap<PathBuf, V>`, `order` position | (case) | cursor memory | ✓ P3.19 |
| fs/mod.rs:370, 462, 506 | Cursor and selection held by `name: String` | (case) | `DirState` | Names come from `read_dir` ✓ P3.20 |
| state/mod.rs:202, 305–318, 357, 367, 382–398 | `dirs: HashMap<PathBuf, Record>` keyed by the pane's path | (case) | app | Persisted keys vs navigated paths ✓ P3.19 |
| state/pins.rs:92–94 | `same_place`: `Path == Path` after `expand_home` | (case) | pins | ✓ P3.19 |
| du/cache.rs:220, 434, 478 | `records: HashMap<PathBuf, …>`, `parent() != Some(dir)`, `starts_with(root)` | (case) | folder sizes | ✓ P3.19 |
| du/scanner.rs:497, 604–614 | `tracked: HashMap<PathBuf, …>` | (case) | du scanner | ✓ P3.19 |
| git/status.rs:180–192, 217–233 | `files`/`dirs`/`collapsed: HashMap<PathBuf, …>`; `status_for` lookup and `ancestors()` | (case) | app:ui.rs:listing, app:grid.rs:paint, app:app.rs:sync_spot, git_line | Keys built from git's spelling joined to `root`, looked up with the pane's spelling ✓ P3.19 |
| git/status.rs:580–585 | `ancestor.starts_with(root)`, `ancestor == root` | (case) | rollup | ✓ P3.19 |
| git/cache.rs:92–93 | `repos`/`roots: HashMap<PathBuf, …>` | (case) | `Git` | ✓ P3.19 |
| sync/plan.rs:51–67 | Duplicate names in `HashSet<OsString>` | (case) | `roots` | ✓ P3.19 |
| sync/plan.rs:307–321 | `ours`/`theirs` `HashSet<&OsString>` | (case), mitigated by `case_twins` | `extras_in` | ✓ P3.19 |
| sync/plan.rs:366–424 | `case_twins`, `fold`: explicit simple case folding (UTF-8 only) | case-aware | `extras_in` | The one place that already models a case-insensitive destination |
| sync/mod.rs:367–383 | `removals`: `HashMap<(usize, &Path), usize>` | (case) | removals | Both sides come from the plan ✓ P3.20 |
| sync/rsync.rs:462–473, 814–839 | Root name maps `HashMap<OsString/&OsStr, usize>`, `items` by `(root, &Path)` | (case) | rsync plan, landed | rsync's spelling vs local names ✓ P3.20 |
| archive/tree.rs:487 | `.git` check with `eq_ignore_ascii_case` | case-aware | `name_is_unsafe` | |
| config.rs:1684–1706 | `wildcard_match` lowercases ASCII text | ASCII case-insensitive | globs | |
| zoxide/mod.rs:315, 320 | `query` lowercases ASCII | ASCII case-insensitive | Z overlay | |
| vfs/config.rs:214, 224 | Service name `==` | (case) | vfs | Config names, not paths ✓ P3.20 |

### 3.5 Path → `String` (lossy) values that are turned back into paths

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| fs/scan.rs:328, 369 | `Entry.name = item.file_name().to_string_lossy()` | Windows-differs (unpaired surrogates); also Unix (non-UTF-8) | `DirState` | |
| fs/mod.rs:506 | `selected_paths`: `self.path.join(n)` over lossy `name`s | same | every op in df-app | The rebuilt path differs from the real one when the name was not valid Unicode |
| config.rs:772; vfs/config.rs:150 | `home.to_string_lossy()` spliced into a path string | same | bookmarks, key file | ✓ P3.14 |
| zoxide/mod.rs:259 | `std::str::from_utf8(raw)` → `PathBuf::from(path)` (non-UTF-8 fails the whole file) | portable | `load_from` | |

### 3.6 Persisted and wire formats that carry a path

| Line | What | Class | Used by | Note (encoding) |
|---|---|---|---|---|
| state/mod.rs:467–510, 512–628 | State file `…/delightfile/state` | Unix-only | app | One record per line, tab-separated. The key is the raw `OsStr` bytes of an absolute path (must start with `/`), escaped (`\\`, `\t`, `\n`, `\r`, `\xNN` for bytes <0x20, `=` and 0x7f), with non-ASCII bytes passed through. Tab records: `!tabs\t<i>=<path bytes>`. Header `# delightfile state v1` |
| state/pins.rs:197–262 | `!pin\tpath=<String>\tkey=<chord>` lines in the same file | portable encoding | app:app/places.rs | `path` is a UTF-8 `String` as typed (`~/Work`, `sftp://…`), escaped with the same `escape`, and expanded by `expand_home` at use |
| ops/trash.rs:649–720 | `.trashinfo`: `[Trash Info]\nPath=<pct-encoded>\nDeletionDate=YYYY-MM-DDThh:mm:ss\n` | Unix-only | trash | `Path=` percent-encodes every `OsStr` byte outside `[A-Za-z0-9-_.~/]`, keeping `/` literal. The date is UTC (729–743). Files are named `<name>.trashinfo`, where `<name>` is `OsStr` bytes clipped to 245 bytes |
| ops/journal.rs | Undo journal | not persisted | app | In-memory `VecDeque<OpRecord>` only |
| vfs/wire.rs, vfs/conn.rs:491–507, 783 | SFTP v3 requests | server-side | vfs | Paths are UTF-8 `String` (`VfsPath.path`) turned into bytes with `into_bytes()`. Names from the server are bytes (`NameEntry.filename`), rendered with `name_lossy()` (wire.rs:444) |
| vfs/mod.rs:1174 | Row paths for remote entries | Windows-differs | df-app | `PathBuf::from("sftp://service/…")` URL string inside a `PathBuf` (§3.7) |
| zoxide/mod.rs:238–275 | `db.zo` (zoxide v3, bincode fixed-int LE) | portable | Z overlay | `u32` version, `u64` count, and per record `u64` len + UTF-8 path bytes + `f64` rank + `u64` last_accessed. Non-UTF-8 fails the whole file |
| preview/cache.rs:136–160 | Thumbnail cache file name | Windows-differs | thumbnails | `format!("{:x}", xxh3_128(bytes))`, where the bytes include std's `Path` `Hash` output (platform-specific) and native-endian integers |
| git/status.rs:363–424 | `git status --porcelain=v2 -z` output | Unix-only | git | NUL-framed. Paths are repo-relative, `/`-separated bytes, and a trailing `/` means a directory |
| git/repo.rs:102–141 | `.git` file `gitdir: <path>` | portable | repo detection | A UTF-8 line. Absolute if `Path::is_absolute`, otherwise joined to the repo root |
| sync/rsync.rs:376–450 | rsync `--out-format=%i %l %n` | Unix-only | rsync plan | `%n` bytes with `\#ooo` octal escapes, folders with a trailing `/` |
| sync/rsync.rs:1143–1211 | Remote `xargs -0 sha256sum` | Unix-only | verify | stdin: NUL-separated name bytes. stdout: `<64 hex>  <name>`, with a leading `\` meaning `\n`/`\r`/`\\` escapes |
| sync/rsync.rs:1174–1184 | Remote script `cd -- '<folder>' && xargs -0 sha256sum --` | Windows-differs | verify | `folder.to_string_lossy()` with `'` → `'\''` |
| sync/rsync.rs:230–244 | rsync endpoints | Unix-only | rsync argv | `host:` + path bytes. Sources lose trailing `/`, and the destination gains one |
| archive/write/mod.rs:522–596 | zip/tar member names | Unix-only | archive creation | `OsStr` bytes joined with `/`. The zip `FLAG_UTF8` bit is defined at write/zip.rs:90 ✓ P3.7 |
| config.rs:155–169 | `[goto]` bookmark paths | portable encoding | app | TOML strings as typed. `~` is expanded at use |

### 3.7 Non-local paths carried in `PathBuf`

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| vfs/mod.rs:1174 | `Entry.path = PathBuf::from(dir.join(&name).to_url())` (`sftp://service/p`) | Windows-differs | df-app rows | `Path::join`/`parent`/`file_name` on such a value use `\` when joining on Windows |
| ops.rs:109–120 | `is_url` detects `<scheme>://` in a `PathBuf` | Unix-only | core:ops/paste.rs:carried, core:sync/plan.rs:roots | |
| ops/paste.rs:56–62 | `carried`: URL kept verbatim, else `normalize` | (via is_url/normalize) | Clipboard, plan_paste | |
| sync/rsync.rs:208–217 | `Transfer { sources: Vec<PathBuf>, dest: PathBuf }` hold server-side paths for a download (sources) or an upload (dest) | Windows-differs | Built at app:app/syncing.rs:remote_sync (469) | |
| sync/rsync.rs:1024–1042 | `far_side`: `name.join(&item.rel)`, `source.parent()` on server paths | Windows-differs | `verify_remote` | |

---

## 4. Spawned processes

| File:line | Binary | Args shape | Blocking? | stdin/stdout handling | Kill/timeout mechanism | Notes |
|---|---|---|---|---|---|---|
| archive/external.rs:207 (`run_in`) | `Extractor.program`: `7z` or `bsdtar` found on `PATH` | 7z: `x -y -aoa\|-aou -o<dest> -- <archive>`. 7z join: `x -tsplit -y -o<into> -- <head>`. bsdtar: `-x -f <archive> -C <dest> [-k]` | Caller thread loops `try_wait` + `sleep(50 ms)` | stdin null. stdout and stderr piped, each drained on its own thread (`drain`), first 64 KiB kept | `ctx.is_cancelled()` → `child.kill()` + `wait()`. `try_wait` error → kill. No timeout | Callers: core:archive/whole.rs:by_extractor, two_stage |
| archive/write/mod.rs:782 (`seven_zip` → `run_in`) | `7z` from `on_path("7z")` | `a -t7z -bd -y -snl -- <temp> <source names…>`, `current_dir` = parent of the first source | as above | as above | as above | `Pack::run` ← app:app/compress.rs:spawn_archive |
| archive/mod.rs:329 (`list_compressed_tar`) | `gzip` / `xz` / `zstd` (bare name) | `-dc` | Yes. `tar::list` reads stdout on the calling thread | stdin = the archive `File` (`Stdio::from(file)`). stdout piped. stderr null | Stop via `Stoppable` (read returns `Err` when `stop()`). Early stop or parse error → `kill()` + `wait()`. Full read → `wait()` and exit status checked | `NotFound` → `ArchiveError::NoDecompressor` |
| archive/mod.rs:393 (`have`) | `gzip` / `xz` / `zstd` | `--version` | Yes (`.status()`) | all null | none | `ArchiveFormat::is_available`, `write::Format::is_available` |
| archive/unpack.rs:600 (`extract_compressed_tar`) | `gzip` / `xz` / `zstd` | `-dc` | Yes. `tar::extract_into` on the calling thread | stdin = archive `File`. stdout piped. stderr null | `Err` (incl. cancel via sink checkpoint) → `kill()` + `wait()`. `Ok` → `wait()` + status | `extract` ← core:ops/jobs.rs:ExtractJob::run |
| archive/write/mod.rs:684 (`piped`) | `zstd` or `xz` (bare name) | zstd: `-q -c -T0`. xz: `-z -c -q -T0` | Yes. The caller thread writes tar bytes into stdin | stdin piped (`BufWriter<ChildStdin>`, written by `tar::write`). stdout = `Stdio::from(File)` (the temp archive). stderr piped and drained on a thread | Cancel or read error → `kill()` + `wait()`. `BrokenPipe` → `wait()` and report stderr. No timeout | `Pack::run` |
| git/status.rs:279 (`status_blocking`) | `git` | `-c core.fsmonitor= -c core.hooksPath=/dev/null -c core.pager=cat -c core.sshCommand= --no-optional-locks status --porcelain=v2 -z --ignored=matching --branch`, `current_dir(root)` | Yes. Runs on the git cache worker thread | stdin null. stdout piped, read on the calling thread up to 64 MiB. stderr piped, read after stdout up to 8 KiB | None. `child.wait()`, no kill, no timeout | Exit: `success()`, `Display` of `ExitStatus` |
| sync/rsync.rs:70 (`available`) | `rsync` | `--version` | Yes (`.status()`) | all null | none | app:app/syncing.rs:remote_sync |
| sync/rsync.rs:316 (`local_version`) | `rsync` | `--version`, env `LC_ALL=C` | Yes (`.output()`) | stdin null, stderr null, stdout captured | none | Gates `--fsync` (`FSYNC_SINCE` = 3.2.0) |
| sync/rsync.rs:518 (`plan` → `collect`) | `rsync` | `-a -n -ii --delete-after --out-format=%i %l %n [--checksum] -e <rsh> -- <src…> <dst/>`, env `LC_ALL=C` | Caller loops `try_wait` + `sleep(50 ms)` | stdin null. stdout read to end on a thread. stderr drained on a thread (64 KiB kept) | `stop()` → `libc::kill(pid, SIGCONT)`, `child.kill()`, `wait()`. No timeout | Exit codes 23/24 count as partial |
| sync/rsync.rs:915 (`run`) | `rsync` | `-a --info=progress2 --no-inc-recursive --out-format=%i %l %n [--delete-after] [--checksum] [--fsync] -e <rsh> -- …`, env `LC_ALL=C` | as above | stdin null. stdout parsed on a thread (`\r` progress lines, `\n` itemized lines). stderr drained | Pause → `libc::kill(pid, SIGSTOP)`, resume → `SIGCONT`, cancel → `SIGCONT` + `kill()`. No timeout | Re-run without `--fsync` if refused |
| sync/rsync.rs:134 (`Host::shell`, via `collect`) | `ssh` (or `Host.program`) | `-x -o BatchMode=yes -o ConnectTimeout=15 [-p N] [-i key] [-l user] -- <host> "cd -- '<folder>' && xargs -0 sha256sum --"`. Program form: `<program> <destination> <script>` | as above | stdin piped. NUL-separated names are written by a thread. stdout captured. stderr drained | As `collect`. `code()` in `{255, 126, 127, None}` → "no sha256sum" | `remote_digests` ← `verify_remote` |
| sync/rsync.rs:173 (`rsh`) — started by rsync, not by df-core | `ssh` | One `-e` string: `'ssh' '-x' '-o' 'BatchMode=yes' '-o' 'ConnectTimeout=15' ['-p' 'N'] ['-i' '<key>'] ['-l' '<user>'] '--'` (single quotes, inner `'` doubled), or the quoted `Host.program` | n/a | n/a | rsync's | |
| vfs/conn.rs:161 (`Transport::spawn`, command from vfs/config.rs:181) | `ssh` (or `Service.program` + args) | `-x -o BatchMode=yes -o ConnectTimeout=15 [-p N] [-i <key>] -s <user@host> sftp` | Blocking per request on the per-service worker thread (vfs/mod.rs:811 `dispatch`), bounded by deadlines | stdin, stdout and stderr piped. stdin and stderr set to `O_NONBLOCK` with `fcntl`. Each read and write is preceded by `libc::poll` | Deadlines: `CONNECT_TIMEOUT` 15 s for the handshake, `OP_TIMEOUT` 30 s per operation → `VfsError::Timeout`. `Drop` → `child.kill()` + `wait()` | SFTP v3 over stdio |

Not spawned by df-core: the `DEFAULT_OPENERS` shell strings (config.rs:192–282) are executed by df-app (app:open.rs). No `sh -c`, `bash`, `xdg-open`, `gio`, `ffmpeg` or `zoxide` process is started by df-core outside tests.

---

## 5. Tests with Unix assumptions

Counts come from a script that classifies each `#[test]` body, plus the non-test helper functions it calls (up to 3 levels), by regex. **abs path** means a string literal starting `"/[a-z]` or `Path::new("/")`. **mode** means `from_mode`, `PermissionsExt`, `set_permissions`, `.mode()` or an `0o…` literal. **symlink** means `symlink(`, `read_link` or `is_symlink`. **uid** means `getuid`, `trash::uid` or `.Trash-N`. **watch** means `Watcher::`, `inotify` or `WatchEvent`. **spawn** means `Command::new`, `"/bin/…"`, `#!/bin/sh`, named tools, `find_sftp_server` or `Service::direct`. **unix API** means `std::os::unix`, `libc::`, `OsStrExt`, `from_bytes` or `from_vec`. **names** means `gnarly_names()` or non-UTF-8 escapes. **dev** means `UnixListener`, `mkfifo` or `/dev/…`. A test is counted once per category. "With any" is the number of tests with ≥1 hit. Files with no hits: archive/volumes.rs (6 tests), archive/write/crc32.rs (2), archive/write/zip.rs (2), keymap/command.rs (3), keymap/key.rs (6), preview/cache.rs (6), preview/job.rs (15), preview/xxh3.rs (7), rename/complete.rs (13), rename/editor.rs (67), rename/exif.rs (20), sha256.rs (9), text.rs (1), thread.rs (2), toml.rs (11).

| Test file | tests | with any | abs path | mode | symlink | uid | watch | spawn | unix API | names | dev | Kind of assumption |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| archive/external.rs | 5 | 4 | 2 | 1 | 0 | 0 | 0 | 2 | 1 | 0 | 0 | Fake `7z`/`bsdtar` files written as `#!/bin/sh` + `0o755` on a synthetic `PATH`. Runs `sh -c "echo …; exit 3"` |
| archive/tests.rs | 73 | 40 | 15 | 20 | 0 | 0 | 0 | 11 | 0 | 0 | 0 | Synthetic zip/tar headers with Unix modes (pure bytes). `/tmp/out`, `/x.zip`, `/etc/cron.d/evil` as pure strings. 11 spawn `gzip`/`xz`/`zstd`/`zip`/`bsdtar`/7-Zip, guarded by `have_binary` |
| archive/whole.rs | 2 | 1 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | Pure string checks (`"a.7z"`). The spawn hit is the string `"7z"`, not a process |
| archive/write/tar.rs | 2 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | Synthetic mode field |
| archive/write/tests.rs | 25 | 16 | 2 | 13 | 11 | 0 | 0 | 13 | 0 | 0 | 0 | Fixture `photos()` sets `0o640`/`0o755` and makes a symlink. Asserts zip/tar mode fields. Runs `zstd`/`xz`/7-Zip/`gunzip` |
| config.rs | 28 | 6 | 6 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `/mnt/…` bookmarks, `/home/brian/Work` icon paths, `expand_home(..).ends_with("/Work")`, `load_from_dir("/nonexistent/…")` |
| du/fstype.rs | 3 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `magic_of(temp dir).is_some()` (Linux `statfs`), `/nonexistent-…` |
| du/tests.rs | 49 | 7 | 4 | 1 | 2 | 0 | 0 | 0 | 2 | 0 | 0 | `st_blocks` via `MetadataExt`, sparse file, hardlink dedupe (`nlink`), symlink skip, `0o000` unreadable dir |
| fs/kind.rs | 11 | 9 | 3 | 9 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | Synthetic `st_mode` values with `S_IF*` socket/fifo/char/block bits and `0o755`. `/tmp` in synthetic `Entry` |
| fs/tests.rs | 53 | 48 | 48 | 40 | 1 | 0 | 3 | 1 | 1 | 0 | 0 | 48 use synthetic `Entry` with `/fixture` paths, `mode: 0o644` and `uid 1000` (no disk). 3 start a real `Watcher` (inotify). 1 makes symlinks with `std::os::unix::fs::symlink`. 1 parses a passwd table |
| fs/typefilter.rs | 4 | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | Synthetic `Entry` |
| git/tests.rs | 35 | 24 | 24 | 0 | 0 | 0 | 0 | 3 | 0 | 0 | 3 | 24 parse synthetic porcelain against root `/repo` (pure). 3 run real `git` with `GIT_CONFIG_GLOBAL=/dev/null` |
| input/tests.rs | 36 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `segment_at("/home/brian/Downloads", …)` with a `/` separator (pure) |
| keymap/tests.rs | 34 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | Bookmark path strings |
| lib.rs | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `DfError::io("/tmp/nope", …)` display (pure) |
| ops.rs | 6 | 5 | 3 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | `normalize("/a/b/../..") == "/"`, `is_absolute`. Symlinks via `TempTree::symlink` |
| ops/copy.rs | 29 | 9 | 1 | 2 | 6 | 0 | 0 | 0 | 4 | 1 | 0 | Mode preservation (`0o640`), `0o555` dir, relative/absolute/broken symlinks (`/nowhere/at/all`), gnarly names |
| ops/create.rs | 10 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | Creates every gnarly name on disk (`\n`, `\t`, `\`, `"`) |
| ops/delete.rs | 12 | 9 | 5 | 0 | 3 | 0 | 0 | 0 | 1 | 1 | 0 | Rails on `/`, `/home`, `/home/brian`. Symlink-to-dir deletion and `link/` trailing-slash behaviour. Gnarly names |
| ops/jobs.rs | 7 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `DeleteJob` on `/` refused |
| ops/journal.rs | 34 | 9 | 3 | 2 | 6 | 0 | 0 | 0 | 5 | 0 | 0 | `0o555` dirs to force undo failure. Symlink undo (`/somewhere/else`). `/tmp/{i}` records |
| ops/link.rs | 13 | 12 | 7 | 0 | 5 | 0 | 0 | 0 | 0 | 1 | 0 | `relative_to` over `/a/b/...` (pure). Symlinks created and read back. Hardlink `(dev, ino)`/`nlink` via `MetadataExt` |
| ops/paste.rs | 37 | 13 | 9 | 0 | 3 | 0 | 0 | 0 | 0 | 1 | 0 | `/tmp/a`, `/tmp/notes.txt` clipboard paths. Paste into itself through a symlink. Gnarly names |
| ops/trash.rs | 31 | 13 | 8 | 2 | 2 | 3 | 0 | 0 | 4 | 3 | 0 | Sticky `.Trash` (`0o1777`), `.Trash-1000`/`-4242`, `mount_point_of("/tmp")`, non-UTF-8 name `caf\xE9` (rejected by APFS), gnarly names, `$HOME` trash |
| preview/kind.rs | 7 | 2 | 2 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | Synthetic `Entry` (`/tmp`, `0o644`) |
| preview/sniff.rs | 17 | 1 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | `#!/bin/sh` as content bytes (pure) |
| preview/syntax.rs | 5 | 2 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | Shebang bytes (pure) |
| rename/facts.rs | 15 | 3 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | `Facts::stat(Path::new("/"), …)`, symlink not followed, `localtime_r` via `Civil::local` |
| rename/template.rs | 34 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | Gnarly names through the template renderer (pure) |
| state/pins.rs | 7 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `/tmp/a`, `/tmp/tab\there…` pin paths, `expand_home` + `/` |
| state/tests.rs | 28 | 22 | 22 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | `/tmp/…`, `/home/brian/…` record keys (the format requires a leading `/`). 1 non-UTF-8 path via `from_vec` |
| sync/rsync.rs | 23 | 16 | 7 | 8 | 0 | 0 | 0 | 11 | 8 | 0 | 0 | Fake `ssh` scripts (`#!/bin/sh … exec sh -c "$*"`, `0o755`, `0o000` locked dir), real `rsync`, `/tmp/a` sources |
| sync/tests.rs | 34 | 8 | 0 | 4 | 3 | 0 | 0 | 0 | 5 | 0 | 1 | `UnixListener` socket as a special file, `0o000`/`0o555` dirs, symlink compare, `MetadataExt` modes |
| tasks.rs | 22 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `"/tmp/x"` in `DfError::io` (pure). `from_raw_os_error(16)`/`(28)` as Linux EBUSY/ENOSPC |
| vfs/tests.rs | 43 | 21 | 18 | 6 | 3 | 0 | 1 | 9 | 2 | 0 | 0 | Replay "servers" as `Service::direct("fake", "/bin/sh", …)`, `/bin/true`. OpenSSH `sftp-server` looked up at `/usr/lib/ssh/…`, `/usr/lib/openssh/…`, `/usr/libexec/…` or `which`. Local `/tmp` fixtures and remote `/etc`/`/srv` strings |
| zoxide/tests.rs | 23 | 16 | 16 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | `/home/brian/…`, `/mnt/plex` db paths (pure), `rsplit('/')` matching |

Cross-cutting test facts:

- **`TempTree::symlink`.** Every test that uses it (test_support.rs:66) needs `std::os::unix::fs::symlink`. It is compiled into df-app's tests through the `test-support` feature.
- **`gnarly_names()`.** It contains `\n`, `\t`, `\` and `"`, which Windows rejects in file names. Tests that create those names on disk are in ops/create.rs, ops/copy.rs, ops/delete.rs, ops/link.rs, ops/paste.rs and ops/trash.rs.
- **`#[cfg]` gating.** No test in the crate carries a `#[cfg(unix)]`, `#[cfg(target_os)]` or `#[ignore]` gate for platform reasons.
