# 04 — Windows

Status: **not started**

Scope: native Windows bodies behind the Phase 1 seam, plus the df-app side of the
path model that Phase 3 did for df-core. The result is the zip from
`06-build-and-release.md` §3.3 running as a file manager on Windows 10/11 (x64):
listing, watching, copy/move, Recycle Bin, clipboard both ways, drop-in, drives
card, openers without a shell, SFTP through Windows OpenSSH, fonts, no console
window. Drag-out is deferred (§8). Every task is "done" at *compiles and unit tests
pass on the Windows runner* and is listed in `07-verification.md` §5 for a human
pass on real hardware or the VM.

Prerequisites: `01-platform-seam.md` and **`03-paths.md` complete** (nothing here
starts before P3.24 is `[x]`), `06-build-and-release.md` B6.4, B6.10–B6.13.

Factual basis: `appendix-inventory-df-core.md` (Unix-only and Windows-differs rows),
`appendix-inventory-df-app.md` §1–§4.

## Decisions already made

- **`windows-sys` only**, pinned to winit's version. COM interfaces are not
  implemented by hand in this phase; that rules out `IFileOperation`, `IShellFolder`
  (Recycle Bin enumeration) and OLE `DoDragDrop`. The consequences are accepted
  below and each is a `[~]` with its design so a later phase can pick it up.
- **Recycle Bin via `SHFileOperationW(FO_DELETE, FOF_ALLOWUNDO)`.** No in-app undo
  of a trash and no trash view on Windows: `u` after a trash toasts "Restore it from
  the Recycle Bin", and the trash tab opens Explorer at `shell:RecycleBinFolder`.
- **Openers are argv lists, no shell.** The `command` string is split on
  whitespace with double-quote grouping; `$1`, `$@` and `$dir` are substituted per
  argument (`$@` expands to as many arguments as there are paths); nothing else is
  expanded — no `%VAR%`, no `$VAR`; a person who wants their `%EDITOR%` writes its
  path into `delightfile.toml`. `open` is a new builtin, `builtin:shell-open`, which is `ShellExecuteW`.
  Typed `;`/`:` commands run through `%COMSPEC% /C` with the paths appended.
- **SFTP pipe I/O is thread-per-pipe on Windows.** The Unix `poll` design stays for
  Unix; on Windows `Transport` owns a reader thread per pipe feeding a channel, and
  `fill` waits with `recv_timeout` against the same deadlines.
- **Drives card lists volumes, ejects removable media, mounts nothing.** Network
  drives are the shares. "Connect to" opens the UNC path in Explorer, which does
  the credential dialog.
- **Console-less subsystem with parent-console attach** for `--help`/`--version`.
- **Hidden files: dot-prefix OR `FILE_ATTRIBUTE_HIDDEN`** (Phase 3 P3.4 already
  implements it; this phase relies on it).
- **DX12 first, WARP accepted.** After the hardware adapter request fails, a second
  `request_adapter` with `force_fallback_adapter: true` lets the app run in a VM.

## 1. Process, console, entry point

- [ ] **W4.1** `main.rs`: `#![cfg_attr(windows, windows_subsystem = "windows")]`
      (the one allowed cfg outside `platform/`, `00-ground-rules.md` §2). Before
      `cli::parse`, call `platform::process::attach_parent_console()` — Windows body
      `AttachConsole(ATTACH_PARENT_PROCESS)` then reopen stdout/stderr handles so
      `Outcome::Print`/`Fail` reach the terminal that launched us; a no-op elsewhere.
      Done when: on the Windows runner `delightfile.exe --version` prints to the
      job log (B6.13) and double-clicking the exe shows no console (live check).
- [ ] **W4.2** `platform::process` Windows bodies: `NULL_DEVICE = "NUL"`;
      `is_executable` = extension is in `PATHEXT` (default `.COM;.EXE;.BAT;.CMD`);
      `candidates("7z")` = `["7z.exe", "7z"]`, `candidates("bsdtar")` = `["bsdtar.exe",
      "tar.exe"]` (Windows ships libarchive's bsdtar as `tar.exe`; GNU-tar flags are
      not an issue there); `exit_code(status)` = `status.code().unwrap_or(-1)`;
      `pause`/`resume` → `Unsupported`; `kill` = `Child::kill` (TerminateProcess).
      Every `Command` df-core and df-app build on Windows for a *console* tool
      (`git`, `7z`, `tar`, `ssh`, `fd`, `rg`) gets `creation_flags(CREATE_NO_WINDOW)`
      through one helper `platform::process::quiet(&mut Command)` (no-op elsewhere),
      so nothing flashes a console. Done when: `git status` on the runner spawns
      without a window (assert the flag in a unit test that inspects the builder,
      since the effect is not observable headlessly).
- [ ] **W4.3** `platform::open` Windows body (S1.26 surface): `shell_argv` is
      replaced on Windows by `argv_from(command: &str, paths: &[PathBuf]) ->
      Vec<OsString>` implementing the Decisions' split-and-substitute rules
      (`$dir` = parent of the first path); `spawn_detached` builds `Command::new(argv[0])`
      with `args`, `creation_flags(CREATE_NEW_PROCESS_GROUP)`, null stdio,
      `current_dir(cwd)`; `run_blocking` the same with `.status()`. `shell_program`
      = `%COMSPEC%` else `cmd.exe`, used only by `App::run_shell` for typed
      commands: `cmd /C <snippet> <paths…>`. `builtin:shell-open` → `ShellExecuteW(NULL,
      L"open", path, NULL, NULL, SW_SHOWNORMAL)` per path, run from `App::launch`'s
      builtin branch (`app.rs:5108–5124`) via `platform::open::shell_open(&Path)`
      (Linux/macOS bodies: `Unsupported`, since their tables never name it). Done
      when: pure tests for the splitter (`zed "$@"` with two paths, `wt -d "$dir"`,
      a quoted path with spaces) pass everywhere; live check V7 §5.7.

## 2. Filesystem (df-core `platform/windows/`)

- [ ] **W4.4** `platform::watch` backend with `ReadDirectoryChangesW`: one
      directory handle per watched dir opened with `FILE_LIST_DIRECTORY |
      FILE_SHARE_READ|WRITE|DELETE | FILE_FLAG_BACKUP_SEMANTICS |
      FILE_FLAG_OVERLAPPED`; filter `FILE_NOTIFY_CHANGE_FILE_NAME | DIR_NAME |
      ATTRIBUTES | SIZE | LAST_WRITE | CREATION`, non-recursive, 64 KiB buffer;
      one `OVERLAPPED` + event per directory and one manual-reset wake event in
      place of the pipe; the thread blocks in `WaitForMultipleObjects`. Any
      completed read → `WatchEvent::Changed(dir)` (the names are not needed; the
      model rescans); `ERROR_NOTIFY_ENUM_DIR` → `Changed(dir)` too;
      `ERROR_ACCESS_DENIED`/`ERROR_FILE_NOT_FOUND` on re-arm (the directory was
      deleted or renamed) → `Gone(dir)` and the handle closes. Done when: a
      Windows-runner test creates a file in a watched temp dir and receives
      `Changed` within one second; another deletes the dir and receives `Gone`.
- [ ] **W4.5** `platform::meta` real bodies (P3.4 specified them; P3.4 may have
      shipped the minimal version): `dev` = `GetVolumeInformationW` serial of the
      path's root, cached per root for the process; `ino` = `0` unless the caller
      asks `platform::meta::identity(path) -> io::Result<(u64, u64)>` which opens the
      file with `FILE_FLAG_BACKUP_SEMANTICS|OPEN_REPARSE_POINT` and reads
      `GetFileInformationByHandle` (used by `same_file` and by du's hard-link dedupe
      only when `nlink > 1`); `nlink` also from `GetFileInformationByHandle`
      (`nNumberOfLinks`) — `std`'s `number_of_links` is unstable — read lazily by
      the du walk through `identity()` only for regular files, since the extra
      handle per entry is what makes it expensive, and defaulting to `1` when the
      open fails; `blocks_bytes` = `GetCompressedFileSizeW` (so compressed
      and sparse files report their allocated size, matching `st_blocks` intent).
      Done when: du tests with hard links pass on the runner.
- [ ] **W4.6** `platform::fs` Windows bodies S1.5 left as stubs (this task
      supersedes S1.5's `writable`): `symlink` maps
      `ERROR_PRIVILEGE_NOT_HELD` (1314) to `DfError::Op("Creating links needs
      Developer Mode or an elevated process")`; `set_times` opens the handle with
      `FILE_FLAG_OPEN_REPARSE_POINT` so a symlink's own times are set;
      `apply_mode` sets/clears `FILE_ATTRIBUTE_READONLY`; `writable(dir)` tries to
      create and delete a `.df-write-test-<pid>` file (there is no `access(W_OK)`
      that respects ACLs). Done when: tests on the runner.
- [ ] **W4.7** `platform::trash` Windows body: `Trash::trash(path)` calls
      `SHFileOperationW` with `wFunc = FO_DELETE`, `pFrom` = the path as a
      double-NUL-terminated wide string — **never** with a `\\?\` prefix, which
      `SHFileOperationW` rejects outright; a path at or over `MAX_PATH` (260) is
      refused before the call with `DfError::Op("path too long for the Recycle Bin")`
      and the app falls back to the permanent-delete confirm as it does for remote
      rows — `fFlags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT |
      FOF_NOERRORUI`; a non-zero return or `fAnyOperationsAborted` is `DfError::Op`
      with the code. It returns a `TrashedItem` whose `location()` is empty and
      the undo journal records `restorable: false` (see W4.8). `list()` →
      `Ok(vec![])`; `restore`/`purge` →
      `Unsupported("Restoring from the Recycle Bin")`. `available_for(path)` = the
      path is on a fixed or removable local drive (`GetDriveTypeW` in
      `{DRIVE_FIXED, DRIVE_REMOVABLE}`; network paths have no Recycle Bin and `d`
      must fall back to the permanent-delete confirm as it does for remote rows).
      Done when: a runner test trashes a temp file and it is gone from the
      directory (the bin itself cannot be inspected headlessly).
- [ ] **W4.8** df-app: `u` after a trash on Windows → the undo journal entry for a
      trash carries `restorable: false` and `App::undo` toasts "Restore it from the
      Recycle Bin" (`ops/journal.rs` gains the flag; Linux/macOS set `true`).
      `App::show_trash` on Windows → `ShellExecuteW("open", "shell:RecycleBinFolder")`
      and a toast "Opened the Recycle Bin in Explorer"; the `trash://` tab is never
      created. "Empty trash" → `SHEmptyRecycleBinW(NULL, NULL, SHERB_NOCONFIRMATION)`
      after the existing confirm card. Done when: `App::for_test` tests on Windows
      assert the toasts.

## 3. Paths in df-app (the df-app rows of Phase 3)

Each task closes the named `appendix-inventory-df-app.md` §1 rows; suffix them
`✓ W4.n`.

- [ ] **W4.9** Breadcrumb: `chrome.rs:853–888` (`crumbs`) uses
      `df_core::path::segments` (P3.2): the first crumb is `C:` (or
      `\\server\share`) and clicking it goes to the drive root. `trashview::crumbs`
      and `remote::crumbs` unchanged. Done when: a `crumbs` test with a
      Windows-shaped path passes (built under `cfg!(windows)`), Linux tests
      unchanged.
- [ ] **W4.10** Roots and fallbacks: `app.rs:16727–16750` (`start_directory`
      fallback), `app.rs:16901–16912` (`nearest_existing`), `app.rs:13048–13075`
      (`copy_piece` Dirname), `archive.rs:106–113` (`Browse::real`), `spot.rs:239–249`
      ("Where" row) → `df_core::path::root_of` / `display`. Done when: no
      `Path::new("/")`/`PathBuf::from("/")` in df-app outside tests (`grep`).
- [ ] **W4.11** Home and `~`: `app.rs:16914–16950` (`typed_path`) accepts `~`,
      `~/x`, `~\x`, `C:\x`, `C:/x`, `\\s\sh`; `finder.rs:336–359` (`shorten_home`)
      strips the home prefix on either separator and renders `~` + the platform
      separator; `app/places.rs:118–196` (`shown`, `brief`, `written`) build from
      `path::segments` rather than splitting on `/`; pins are still written as
      `~/…` with `/` (portable file, `expand_home` keeps whatever was typed).
      `trashview.rs:276–293` (`shorten`) shares `shorten_home`. Done when: tests
      for each with Windows-shaped inputs.
- [ ] **W4.12** Names: `app.rs:16752–16770` (`save_target`), `bulk.rs:1306–1331`
      (`problems`) use `df_core::path::name_is_valid` (P3.2, strict on Windows);
      `bulk.rs:126–130` `NAME_MAX` becomes `df_core::platform::path::MAX_NAME`
      (255 UTF-16 units on Windows). The bulk card's row problem text names the
      offending character. Done when: `problems(["con"])` is `Unusable` on Windows
      and `None` on Linux in tests.
- [ ] **W4.13** Separators in output parsing: `search.rs:686–731` (`parse`) trims
      either separator from `fd` output; `overlay.rs:727–760` (`path_text`) splits
      on the last of either separator; `dnd.rs:670–696` (`paths_from` plain-text
      fallback) keeps lines where `Path::new(line).is_absolute()`;
      `clipboard.rs:226–261` (`parse_file_uri`) turns `file:///C:/x` into `C:\x`
      (drop the leading `/` when the next two characters are a drive letter and
      `:`) and `file_uri` emits `file:///C:/x` (forward slashes, drive kept). Done
      when: round-trip tests for `C:\Users\a b\x.txt` ↔ `file:///C:/Users/a%20b/x.txt`.
- [ ] **W4.14** Remote rows: audit every `Path::join`/`parent`/`file_name` applied
      to an `Entry.path` while `remote::is_remote(path)` (appendix B `remote.rs:107–122`
      row lists the entry points: `scannable`, `spawnable_cwd`, `child_cwd`,
      `jump_to`, `remote_at`) and route them through `VfsPath` (`at_of` first).
      `app/syncing.rs:411–424` (`server_path`) returns `String`, not `PathBuf`.
      Done when: a Windows-runner test navigates a fake remote listing (the
      `vfs/tests.rs` replay server is Unix-only, so use a `Session` with cached
      rows) and the derived child URL has no `\`.
- [ ] **W4.15** `cli.rs:449–478` (`write_cwd_file`, `write_chooser_file`) use
      `as_encoded_bytes` which is WTF-8 on Windows; document that the wrapper
      protocol is UTF-8 and leave the code. Done when: the doc comment says so.

## 4. Clipboard and drop (df-app `platform/windows/`)

- [ ] **W4.16** `platform::clipboard` bodies: `OpenClipboard(hwnd)` with a retry
      loop (up to 10 × 10 ms, since another app may hold it), `EmptyClipboard`,
      then per branch: text → `CF_UNICODETEXT` (`HGLOBAL` of UTF-16 + NUL);
      `text/uri-list` → `CF_HDROP` (`DROPFILES { pFiles: size_of::<DROPFILES>(),
      fWide: 1 }` followed by the wide paths, double-NUL) **and** the registered
      format `Preferred DropEffect` = `DROPEFFECT_COPY` so Explorer copies rather
      than moves; `image/png` → the registered format `PNG` with the raw bytes
      (Explorer and Paint read it) and no DIB conversion. `CloseClipboard` on every
      path. Returns `Ok(None)`. `offered_types()` → `EnumClipboardFormats` mapped:
      `CF_HDROP` → `text/uri-list`, `CF_UNICODETEXT` → `text/plain;charset=utf-8`,
      `PNG` → `image/png`. `paste("text/uri-list")` → `DragQueryFileW` over the
      `CF_HDROP` handle rendered through `clipboard::uri_list` (with W4.13's
      `file_uri`); `paste(text)` → `CF_UNICODETEXT`; `paste("image/png")` → `PNG`.
      Done when: a runner test round-trips text and one path through the real
      clipboard (the runner has a desktop session; guard with `#[ignore]` if it
      proves flaky and say so).
- [ ] **W4.17** `platform::desktop::pointer_position(&Window)`: `GetCursorPos` +
      `ScreenToClient(hwnd)` divided by the scale factor. `Desktop::start` returns a
      handle whose `set_selection`/`receive` delegate to W4.16 synchronously (as
      M2.13 does on macOS) and whose `drag` returns `false` (§8). Done when: a drop
      from Explorer lands on the row under the pointer (live check V7 §5.5).

## 5. Drives card (df-app `platform/windows/mounts.rs`)

- [ ] **W4.18** `Request::List` → for each bit of `GetLogicalDrives`:
      `GetDriveTypeW(X:\)`; `DRIVE_FIXED|DRIVE_REMOVABLE|DRIVE_CDROM` → `Device {
      object: "X:", drive: None, node: "X:\", label: GetVolumeInformationW name or
      "Local Disk", fs: the filesystem name, size: GetDiskFreeSpaceExW total,
      mount: Some("X:\"), removable: type == REMOVABLE, ejectable: REMOVABLE|CDROM,
      hardware: "" }`; `DRIVE_REMOTE` → `Share { url: WNetGetConnectionW UNC, label:
      "X: → \\server\share", scheme: "smb", path: "X:\" }`. `Eject(X:)` →
      `CreateFileW("\\\\.\\X:")` + `DeviceIoControl(FSCTL_LOCK_VOLUME)`,
      `FSCTL_DISMOUNT_VOLUME`, `IOCTL_STORAGE_MEDIA_REMOVAL(prevent=false)`,
      `IOCTL_STORAGE_EJECT_MEDIA`; `Unmount` → `Reply::Failed("Windows has no
      unmount; use Eject for removable drives")`; `Mount` → `Failed`;
      `UnmountShare(X:)` → `WNetCancelConnection2W`. Done when: the mapping has
      unit tests with a fake table; live check V7 §5.7.
- [ ] **W4.19** `connect(url)`: `smb://server/share` → `ShellExecuteW("open",
      "\\\\server\\share")` (Explorer prompts for credentials) →
      `Connected::Mounted(Some(UNC as PathBuf))`; other schemes →
      `Connected::Failed("Windows opens smb:// shares only")`. `TERMINAL_MOUNT` is
      `None`. Done when: live check.

## 6. SFTP (df-core `platform/windows/pipe.rs` and `vfs/conn.rs`)

- [ ] **W4.20** `platform::pipe` Windows design: `Transport::spawn` (`vfs/conn.rs:161–188`)
      keeps `stdin` synchronous and starts two threads, `df-sftp-out` and
      `df-sftp-err`, each looping `read` into 64 KiB chunks sent over a
      `crossbeam_channel::Sender<Vec<u8>>` (bounded 64). `fill` (`:271–308`) becomes
      `recv_timeout(deadline - now)` + append to the buffer; `drain_stderr` drains
      its channel with `try_recv`; `write_all` (`:337–367`) writes synchronously (a
      blocked write past the deadline is detected by a watchdog `Instant` check on
      the next call and reported as `VfsError::Timeout`). On Unix nothing changes:
      the two designs live behind `platform::pipe::Reader` with `fn open(stdout,
      stderr) -> Reader` and `fn read_until(&mut self, deadline) -> Result<Chunk>`,
      where the Unix `Reader` is the existing `poll` code. `Drop` kills the child,
      which unblocks the threads. Done when: `vfs/tests.rs`'s replay tests get a
      Windows twin using a small Rust replay binary (`tests/bin/replay.rs`) instead
      of `/bin/sh`, and pass on the runner.
- [ ] **W4.21** `vfs/config.rs:181–202` (`Service::command`): `ssh` resolves through
      `candidates("ssh")` (`ssh.exe` from Windows OpenSSH is on `PATH` by default);
      `-i ~/.ssh/key` expands `~` with `platform::dirs::home()`; `quiet(&mut cmd)`
      applied. `vfs/mod.rs:657–668` temp names sanitized (P3.21). Done when: live
      check V7 §5.7.

## 7. Fonts, graphics, dirs

- [ ] **W4.22** `platform::fonts::dirs()` Windows list per `05-defaults-and-config.md`
      D5.6. Done when: D5.6 done.
- [ ] **W4.23** `graphics.rs:77–115`: after the hardware `request_adapter` fails on
      Windows, retry with `force_fallback_adapter: true` and log "using the
      software adapter (WARP)". Guarded by `platform::gfx::ALLOW_FALLBACK_ADAPTER`
      (Windows `true`). Done when: the app draws in a VM without GPU passthrough
      (live check V7 §3).
- [ ] **W4.24** `platform::dirs` Windows values (D5.1). Done when: D5.1 done.
- [ ] **W4.25** Keyboard: AltGr. On a layout where `@`/`€` need AltGr, winit reports
      Ctrl+Alt held. `platform::keys::mods` on Windows: when both `control_key()`
      and `alt_key()` are set **and** the event carries printable `text`, treat the
      press as unmodified text (chord `None`) so `route_keys` types it. This covers
      the other "modifier held → not text" sites too (`bulk.rs:1246`,
      `app.rs:8151–8153, 8190–8194`), because they look at the chord's `Mods`, which
      are now empty. Done when:
      a `keys.rs` test with `CONTROL|ALT` and `text: "@"` yields no chord under
      `cfg!(windows)`; live check V7 §5.2.
- [ ] **W4.29** Permissions and owner off-Linux: `spot.rs:116–130, 150–200,
      268–275, 369–383, 532` (nine POSIX mode chips, the Owner row, `SetMode`) and
      `format.rs:19–33` (`Permissions`/`Owner` linemodes). On Windows the spot
      panel shows one chip, **Read-only**, toggling `0o222` through
      `platform::fs::apply_mode` (which maps it to `FILE_ATTRIBUTE_READONLY`); the
      Owner row is omitted; the `Permissions` linemode shows `ro`/`rw` plus `h`
      (hidden) and `s` (system) letters from the attributes; `Owner` linemode shows
      `—`. Implemented by `platform::meta::PermissionModel { Posix, ReadOnlyFlag }`
      consulted by `spot::rows`/`BITS` and `format::linemode_text`. macOS keeps the
      POSIX model. Done when: `spot` tests under `cfg!(windows)` see one chip.
- [ ] **W4.30** `trash://` and `sftp://` pseudo-paths compared as `Path` (`tab.rs:770–790`,
      `trashview.rs:72–78`, `app.rs:2486, 6785, 16555`): add a unit test that
      `Path::new("trash://") == PathBuf::from("trash://")` and that
      `PathBuf::from("sftp://h/a").starts_with("sftp://h")` hold on Windows (they
      should, since neither has a drive prefix and equality is component-wise), so
      the assumption is checked rather than believed. Done when: the test passes on
      the runner.
- [ ] **W4.26** Tests: Windows twins for `crumbs`, `typed_path`, `shorten_home`,
      `save_target`, `parse_file_uri`; `#[cfg(unix)]` on the df-app tests appendix B
      §7 flags that S1.33 did not already gate. Done when: `cargo test -p df-app`
      green on the runner.

## 8. Deferred, with the design recorded

- [~] **W4.27** Drag-out via OLE — skipped: needs `IDataObject` and `IDropSource`
      COM objects. Design when picked up: `OleInitialize` on the UI thread at
      startup; a hand-written vtable for `IDataObject` serving `CF_HDROP` +
      `Preferred DropEffect` (reuse W4.16's builders) and `IDropSource` whose
      `QueryContinueDrag` returns `DRAGDROP_S_DROP` on button release and
      `DRAGDROP_S_CANCEL` on Escape; `DoDragDrop(data, source, DROPEFFECT_COPY,
      &mut effect)` called from the `CursorMoved` arm (as M2.12 does) since it
      blocks in a modal loop until the drop; on return push `Event::DragEnded`.
      The `windows` crate's `#[implement]` macro would make this a page of code
      instead of three, which is the argument for revisiting the dependency
      policy for this one feature.
- [~] **W4.28** In-app Recycle Bin restore and trash view — skipped: needs
      `IShellFolder` enumeration of `CSIDL_BITBUCKET` and `IContextMenu`
      "undelete". Same note as W4.27.

## Decisions log

- 2026-09-25 — No hand-written COM; drag-out and Recycle Bin restore deferred.
- 2026-09-25 — Argv openers with `$1`/`$@`/`$dir`; `builtin:shell-open`.
- 2026-09-25 — Thread-per-pipe SFTP transport on Windows; Unix keeps `poll`.
- 2026-09-25 — WARP fallback adapter allowed on Windows only.

## Open questions

- Whether the Windows runner's clipboard tests are stable enough to be required
  (W4.16); decide after the first ten runs.
- Whether `wt.exe` is a safe default terminal (`05-defaults-and-config.md` §2): it
  is absent on a fresh Windows 10 LTSC.
