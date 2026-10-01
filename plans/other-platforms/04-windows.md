# 04 — Windows

Status: **in progress**

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

- [x] **W4.1** `main.rs`: `#![cfg_attr(windows, windows_subsystem = "windows")]`
      (the one allowed cfg outside `platform/`, `00-ground-rules.md` §2). Before
      `cli::parse`, call `platform::process::attach_parent_console()` — Windows body
      `AttachConsole(ATTACH_PARENT_PROCESS)` then reopen stdout/stderr handles so
      `Outcome::Print`/`Fail` reach the terminal that launched us; a no-op elsewhere.
      Done when: on the Windows runner `delightfile.exe --version` prints to the
      job log (B6.13) and double-clicking the exe shows no console (live check).
      — done 5722b0a: `platform::process::attach_parent_console` (the Linux
      and macOS body a no-op in `unix/`), standard output and error reopened
      on `CONOUT$` only when they point nowhere, so a pipe or a file the
      parent gave is kept; the attribute is `cfg_attr(all(windows,
      not(test)))`, the unit-test build staying a console program. The Windows
      job's smoke test now checks what `--version` printed (`delightfile
      0.1.0`, run 36662477949), `tests/version.rs` checks `--version` and
      `--help` through a pipe on every target, and on the VM a double-click
      opened no console (07 §5.8).
- [x] **W4.2** `platform::process` Windows bodies: `NULL_DEVICE = "NUL"`;
      `is_executable` = extension is in `PATHEXT` (default `.COM;.EXE;.BAT;.CMD`);
      `candidates("7z")` = `["7z.exe", "7z"]`, `candidates("bsdtar")` = `["bsdtar.exe",
      "tar.exe"]` (Windows ships libarchive's bsdtar as `tar.exe`; GNU-tar flags are
      not an issue there); `exit_code(status)` = `status.code()`, 1 when there is none (written in S1.26);
      `pause`/`resume` → `Unsupported`; `kill` = `Child::kill` (TerminateProcess).
      Every `Command` df-core and df-app build on Windows for a *console* tool
      (`git`, `7z`, `tar`, `ssh`, `fd`, `rg`) gets `creation_flags(CREATE_NO_WINDOW)`
      through one helper `platform::process::quiet(&mut Command)` (no-op elsewhere),
      so nothing flashes a console. Done when: `git status` on the runner spawns
      without a window (assert the flag in a unit test that inspects the builder,
      since the effect is not observable headlessly).
      — done ef98bea (and D5.7's 45cff54), green on the Windows runner at run
      36666369775; df-core's half — df-app's console tools are W4.38, and
      `CREATE_NEW_PROCESS_GROUP` stays with W4.3's openers (Decisions log)
- [x] **W4.3** `platform::open` Windows body (S1.26 surface): `shell_argv` is
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
      — done ddafa4b, d3bf75c and 4d56ecf: the splitter is `platform/argv.rs`
      (compiled on Windows and in every target's tests);
      `spawn_detached`/`run_blocking` run an opener's argv in a process group
      of its own; a typed line goes through `spawn_typed`/`run_typed`,
      `%COMSPEC% /S /C "<line> <paths…>"` with the line as a raw argument,
      under `CREATE_NO_WINDOW`, and between `pushd` and `popd` when the folder
      is a share (Decisions log), so the window says whose line it runs
      (`open::Line`); `shell_open` is `ShellExecuteW` "open", with COM started
      on the calling thread for the call (9be302e). `first_available` is not
      written: its callers are the Windows opener table, D5.3, which is
      df-core's (05 D5.4).

## 2. Filesystem (df-core `platform/windows/`)

- [x] **W4.4** `platform::watch` backend with `ReadDirectoryChangesW`: one
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
      — done c0196a8, b13cd9d, 687767a (and 1eda358, the kqueue test's own wait for
      the bell), green on the Windows runner at run 36666369775:
      `platform::windows::watch::tests` see a file land within a second, a deleted
      folder `Gone` and let go, and the watched set replaced; `fs::tests`' two
      watcher tests run on Windows too
- [x] **W4.5** `platform::meta` real bodies (P3.4 specified them; P3.4 may have
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
      Done when: du tests with hard links pass on the runner. — done in Phase 3
      (P3.4, 76d362e), ahead of this phase because ten df-core tests on the runner
      hung on it: `identity(path, &meta) -> io::Result<Identity { dev, ino,
      nlink }>`, all three from one `GetFileInformationByHandle` (so `dev` is the
      volume serial of the file's own handle, not a per-root cache), asked by
      `same_file` and, for regular files only (`maybe_linked`), by the du
      walk's hard-link dedupe. `blocks_bytes` is split out as W4.35.
- [x] **W4.6** `platform::fs` Windows bodies S1.5 left as stubs (this task
      supersedes S1.5's `writable`): `symlink` maps
      `ERROR_PRIVILEGE_NOT_HELD` (1314) to `DfError::Op("Creating links needs
      Developer Mode or an elevated process")`; `set_times` opens the handle with
      `FILE_FLAG_OPEN_REPARSE_POINT` so a symlink's own times are set;
      `apply_mode` sets/clears `FILE_ATTRIBUTE_READONLY`; `writable(dir)` tries to
      create and delete a `.df-write-test-<pid>` file (there is no `access(W_OK)`
      that respects ACLs). Done when: tests on the runner.
      — done 9cb7555, green on the Windows runner at run 36666369775: link kinds,
      times on a link and a folder, the read-only attribute, the `writable` probe
- [x] **W4.7** `platform::trash` Windows body: `Trash::trash(path)` calls
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
      — done 304fcf7, green on the Windows runner at run 36666369775: a trashed temp
      file leaves its folder and the shell's own count of the bin goes up
      (`SHQueryRecycleBinW`); fixed drives only (Decisions log)
      — and 44429aa: `FOF_WANTNUKEWARNING`, so a file too big for the bin is
      deleted for good only after the shell's own question (Decisions log,
      green on the Windows runner at run 36737465177,
      `a_recycle_asks_before_it_deletes_for_good`)
- [x] **W4.8** df-app: `u` after a trash on Windows → the undo journal entry for a
      trash carries `restorable: false` and `App::undo` toasts "Restore it from the
      Recycle Bin" (`ops/journal.rs` gains the flag; Linux/macOS set `true`).
      `App::show_trash` on Windows → `ShellExecuteW("open", "shell:RecycleBinFolder")`
      and a toast "Opened the Recycle Bin in Explorer"; the `trash://` tab is never
      created. "Empty trash" → `SHEmptyRecycleBinW(NULL, NULL, SHERB_NOCONFIRMATION)`
      after the existing confirm card. Done when: `App::for_test` tests on Windows
      assert the toasts.
      — blocked in part: de6769e does the df-app half against the contract
      W4.7 publishes on `port/windows-core` (a `TrashedItem` whose
      `location()` is empty): `u` after a trash says "Restore it from the
      Recycle Bin" and takes nothing back
      (`platform::trash::RESTORED_ELSEWHERE`; the journal has no `restorable`
      flag, being df-core's, and every Windows trash is unrestorable here),
      and the trash's door opens `shell:RecycleBinFolder` in Explorer with
      "Opened the Recycle Bin in Explorer" (`SYSTEM_BIN`);
      `undo_after_a_trash_says_where_to_restore_it_on_windows`. Both stay
      behind the "Trash is not available on this platform" refusal until W4.7
      is merged. "Empty trash" is blocked on a choice: Open questions.
      — "Empty trash", option (b), decided (Decisions log): df-core's half
      is `platform::trash::{bin_size, empty_bin}` (port/windows-core), the
      bins counted by `SHQueryRecycleBinW` and emptied by
      `SHEmptyRecycleBinW(…, SHERB_NOCONFIRMATION)`; the count-only card
      ("Empty the Recycle Bin? N items, X") is df-app's, still to do. The
      df-core half: 704ea0b, green on the Windows runner at run 36666369775
      (`the_bin_is_counted_and_emptied`); 645bfbc turns df-app's "the trash
      is refused" test into one that no platform refuses it.
      — done 2225ac7, green on the Windows runner at run 36737465177: "Empty
      trash" opens `ConfirmKind::EmptyBin`, "Empty the Recycle Bin? N items
      · X will be deleted for good.", from `bin_size()`, and its yes runs
      `empty_bin()` as a job; the palette offers the row on Windows in any
      folder; the gate that refused a platform with no trash is gone
      (`empty_trash_counts_the_bin_where_the_system_keeps_it`,
      `the_trash_is_there_on_every_platform`, Decisions log). Not seen on
      the VM (07 §5.8).

## 3. Paths in df-app (the df-app rows of Phase 3)

Each task closes the named `appendix-inventory-df-app.md` §1 rows; suffix them
`✓ W4.n`.

- [x] **W4.9** Breadcrumb: `chrome.rs:853–888` (`crumbs`) uses
      `df_core::path::segments` (P3.2): the first crumb is `C:` (or
      `\\server\share`) and clicking it goes to the drive root. `trashview::crumbs`
      and `remote::crumbs` unchanged. Done when: a `crumbs` test with a
      Windows-shaped path passes (built under `cfg!(windows)`), Linux tests
      unchanged. — done 0ff9749 in Phase 3, with the path model the
      breadcrumb is a site of: `crumbs` is `path::segments` mapped, which walks a
      `.`/`..` without offering it as the old loop did, so Linux draws the same
      crumbs; `a_windows_breadcrumb_starts_at_the_drive` (a drive, its root, a
      share), and the Unix test runs under `cfg!(unix)`.
- [x] **W4.10** Roots and fallbacks: `app.rs:16727–16750` (`start_directory`
      fallback), `app.rs:16901–16912` (`nearest_existing`), `app.rs:13048–13075`
      (`copy_piece` Dirname), `archive.rs:106–113` (`Browse::real`), `spot.rs:239–249`
      ("Where" row) → `df_core::path::root_of` / `display`. Done when: no
      `Path::new("/")`/`PathBuf::from("/")` in df-app outside tests (`grep`). —
      done 0ff9749 in Phase 3: `nearest_existing`, `copy_piece`'s dirname
      and `Browse::real` fall back to `root_of` the path, the Where row to its
      `display`; `start_directory` and `reveal_directory`, which have no path
      to take a root from when the working directory is gone, fall back to
      `MAIN_SEPARATOR_STR` (`/` on Linux, as before). The grep's hits left
      are all test code (`mounts.rs:2606` is a `#[cfg(test)]` fixture).
- [x] **W4.11** Home and `~`: `app.rs:16914–16950` (`typed_path`) accepts `~`,
      `~/x`, `~\x`, `C:\x`, `C:/x`, `\\s\sh`; `finder.rs:336–359` (`shorten_home`)
      strips the home prefix on either separator and renders `~` + the platform
      separator; `app/places.rs:118–196` (`shown`, `brief`, `written`) build from
      `path::segments` rather than splitting on `/`; pins are still written as
      `~/…` with `/` (portable file, `expand_home` keeps whatever was typed).
      `trashview.rs:276–293` (`shorten`) shares `shorten_home`. Done when: tests
      for each with Windows-shaped inputs.
      — done d27a843: `typed_path` splits `~` on either separator and gives a
      drive typed alone its root (`D:` is `D:\`); `shorten_home` takes either
      separator as the boundary and keeps the one the path was spelled with;
      the Places card's `brief` and `folder_name` split a local label on the
      platform's separators and a URL on `/`; `written` puts a pin under `~`
      with `/` (`path::with_slashes`); the trash view's `shorten` is
      `shorten_home`. Windows twins:
      `a_typed_path_resolves_like_a_shell_would_on_windows`,
      `the_home_prefix_shortens_on_windows`,
      `a_windows_place_is_briefed_named_and_written_with_its_separators`.
- [x] **W4.12** Names: `app.rs:16752–16770` (`save_target`), `bulk.rs:1306–1331`
      (`problems`) use `df_core::path::name_is_valid` (P3.2, strict on Windows);
      `bulk.rs:126–130` `NAME_MAX` becomes `df_core::platform::path::MAX_NAME`
      (255 UTF-16 units on Windows). The bulk card's row problem text names the
      offending character. Done when: `problems(["con"])` is `Unusable` on Windows
      and `None` on Linux in tests.
      — done 770943f, which is P3.31's option (a): `Problem::Unusable(Why)`
      says which character is to blame (`cannot contain :`), or the platform's
      own words for a trailing dot or a device name; `NAME_MAX` is
      `platform::os::MAX_NAME` counted in UTF-16 on Windows; `save_target`
      refuses what `name_is_valid` refuses.
      `a_name_windows_will_not_make_is_unusable_there_only` and
      `a_save_name_is_a_name_windows_can_make`.
- [x] **W4.13** Separators in output parsing: `search.rs:686–731` (`parse`) trims
      either separator from `fd` output; `overlay.rs:727–760` (`path_text`) splits
      on the last of either separator; `dnd.rs:670–696` (`paths_from` plain-text
      fallback) keeps lines where `Path::new(line).is_absolute()`;
      `clipboard.rs:226–261` (`parse_file_uri`) turns `file:///C:/x` into `C:\x`
      (drop the leading `/` when the next two characters are a drive letter and
      `:`) and `file_uri` emits `file:///C:/x` (forward slashes, drive kept). Done
      when: round-trip tests for `C:\Users\a b\x.txt` ↔ `file:///C:/Users/a%20b/x.txt`.
      — done 9594ced and 654b5af: a hit is named in the platform's separator
      whichever one fd or rg wrote (`search::native`), which is what the six
      hits tests the runner failed needed; `path_text` splits at either
      separator; `file_uri`/`parse_file_uri` write and read `file:///C:/x` and
      `file://server/share/x`
      (`windows_paths_round_trip_as_windows_writes_them`). The plain-text
      drop's `paths_from` is not changed: M2.31 moved it to
      `platform/linux/wayland/incoming.rs`, where `/` is what absolute means,
      and a drop from Explorer arrives as winit's `DroppedFile` paths (W4.17).
- [x] **W4.14** Remote rows: audit every `Path::join`/`parent`/`file_name` applied
      to an `Entry.path` while `remote::is_remote(path)` (appendix B `remote.rs:107–122`
      row lists the entry points: `scannable`, `spawnable_cwd`, `child_cwd`,
      `jump_to`, `remote_at`) and route them through `VfsPath` (`at_of` first).
      `app/syncing.rs:411–424` (`server_path`) returns `String`, not `PathBuf`.
      Done when: a Windows-runner test navigates a fake remote listing (the
      `vfs/tests.rs` replay server is Unix-only, so use a `Session` with cached
      rows) and the derived child URL has no `\`.
      — done 5e89119: every remote path df-app makes is `VfsPath`'s join or
      parent already, bar one, the sync card's destination label, now joined
      through `VfsPath`; `server_path` returns `String`;
      `a_remote_folder_is_entered_and_left_by_its_url` walks a cached listing
      and finds no `\` in any URL.
- [x] **W4.15** `cli.rs:449–478` (`write_cwd_file`, `write_chooser_file`) use
      `as_encoded_bytes` which is WTF-8 on Windows; document that the wrapper
      protocol is UTF-8 and leave the code. Done when: the doc comment says so.
      — done 65c8b50.

## 4. Clipboard and drop (df-app `platform/windows/`)

- [x] **W4.16** `platform::clipboard` bodies: `OpenClipboard(hwnd)` with a retry
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
      — done 742fa79: `CF_UNICODETEXT`, `CF_HDROP` + `Preferred DropEffect`
      (copy), the registered `PNG`; a non-PNG picture is made a PNG on a copy
      and a `CF_DIBV5`/`CF_DIB` one on a paste (`platform/clipformats.rs`,
      tested on every target); the clipboard is opened for the window once
      there is one (`own_with`).
      `text_and_files_round_trip_through_the_clipboard` and
      `a_png_round_trips_under_its_own_format` pass on the runner
      (36662477949), not ignored.
- [x] **W4.17** `platform::desktop::pointer_position(&Window)`: `GetCursorPos` +
      `ScreenToClient(hwnd)` divided by the scale factor. `Desktop::start` returns a
      handle whose `set_selection`/`receive` delegate to W4.16 synchronously (as
      M2.13 does on macOS) and whose `drag` returns `false` (§8). Done when: a drop
      from Explorer lands on the row under the pointer (live check V7 §5.5).
      — done 390e380: `Desktop` answers each request on the next `poll`,
      mirrors the clipboard by `GetClipboardSequenceNumber`, refuses a drag;
      `pointer_position` is `GetCursorPos` + `ScreenToClient` over the scale.

## 5. Drives card (df-app `platform/windows/mounts.rs`)

- [x] **W4.18** `Request::List` → for each bit of `GetLogicalDrives`:
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
      — done 9be302e: the mapping is `platform/drives.rs` (tested with a fake
      table on every target); a letter with no medium is not listed; the
      listing finds the runner's system drive (`the_system_drive_is_listed`).
- [x] **W4.19** `connect(url)`: `smb://server/share` → `ShellExecuteW("open",
      "\\\\server\\share")` (Explorer prompts for credentials) →
      `Connected::Mounted(Some(UNC as PathBuf))`; other schemes →
      `Connected::Failed("Windows opens smb:// shares only")`. `TERMINAL_MOUNT` is
      `None`. Done when: live check.
      — done 9be302e and 214488a: the connect waits up to 10 s, on its pool
      thread, for the UNC path to answer before going there, so the window
      never waits on a network timeout; `Mounted(None)` after that, which says
      "Explorer was asked to connect to …" (`mounts::CONNECT_UNSEEN`, as M2.15
      on macOS).

## 6. SFTP (df-core `platform/windows/pipe.rs` and `vfs/conn.rs`)

- [x] **W4.20** `platform::pipe` Windows design: `Transport::spawn` (`vfs/conn.rs:161–188`)
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
      — done cea654b, b108141, green on the Windows runner at run 36666369775: the
      five fake-server tests and a Windows twin of the reconnect test run there
      against the `sftp_replay` example
- [x] **W4.21** `vfs/config.rs:181–202` (`Service::command`): `ssh` resolves through
      `candidates("ssh")` (`ssh.exe` from Windows OpenSSH is on `PATH` by default);
      `-i ~/.ssh/key` expands `~` with `platform::dirs::home()`; `quiet(&mut cmd)`
      applied. `vfs/mod.rs:657–668` temp names sanitized (P3.21). Done when: live
      check V7 §5.7.
      — done 989e498, green on the Windows runner at run 36666369775 (the real
      `ssh.exe` against a closed port); the live check is V7 §5.7

## 7. Fonts, graphics, dirs

- [ ] **W4.22** `platform::fonts::dirs()` Windows list per `05-defaults-and-config.md`
      D5.6. Done when: D5.6 done.
- [x] **W4.23** `graphics.rs:77–115`: after the hardware `request_adapter` fails on
      Windows, retry with `force_fallback_adapter: true` and log "using the
      software adapter (WARP)". Guarded by `platform::gfx::ALLOW_FALLBACK_ADAPTER`
      (Windows `true`). Done when: the app draws in a VM without GPU passthrough
      (live check V7 §3).
      — done 4e6adfa: the retry is `find_adapter(…, software: true)` on the
      DX12 instance, before the every-backend retry; and whatever request
      found it, a CPU adapter is logged as WARP, since wgpu ranks a software
      adapter last but takes it when it is all there is, which is how the VM
      got one from the first request.
- [x] **W4.24** `platform::dirs` Windows values (D5.1). Done when: D5.1 done.
      — done f2196c8, green on the Windows runner at run 36666369775 (D5.1's Windows
      column)
- [x] **W4.25** Keyboard: AltGr. On a layout where `@`/`€` need AltGr, winit reports
      Ctrl+Alt held. `platform::keys::mods` on Windows: when both `control_key()`
      and `alt_key()` are set **and** the event carries printable `text`, treat the
      press as unmodified text (chord `None`) so `route_keys` types it. This covers
      the other "modifier held → not text" sites too (`bulk.rs:1246`,
      `app.rs:8151–8153, 8190–8194`), because they look at the chord's `Mods`, which
      are now empty. Done when:
      a `keys.rs` test with `CONTROL|ALT` and `text: "@"` yields no chord under
      `cfg!(windows)`; live check V7 §5.2.
      — done 3b368bd: `platform::keys::composed`; winit 0.30.13 already leaves
      Ctrl and Alt out while the right Alt is AltGr, so this covers the left
      Ctrl+Alt spelling. `ctrl_alt_with_text_is_altgr_on_windows_only`.
- [x] **W4.29** Permissions and owner off-Linux: `spot.rs:116–130, 150–200,
      268–275, 369–383, 532` (nine POSIX mode chips, the Owner row, `SetMode`) and
      `format.rs:19–33` (`Permissions`/`Owner` linemodes). On Windows the spot
      panel shows one chip, **Read-only**, toggling `0o222` through
      `platform::fs::apply_mode` (which maps it to `FILE_ATTRIBUTE_READONLY`); the
      Owner row is omitted; the `Permissions` linemode shows `ro`/`rw` plus `h`
      (hidden) and `s` (system) letters from the attributes; `Owner` linemode shows
      `—`. Implemented by `platform::meta::PermissionModel { Posix, ReadOnlyFlag }`
      consulted by `spot::rows`/`BITS` and `format::linemode_text`. macOS keeps the
      POSIX model. Done when: `spot` tests under `cfg!(windows)` see one chip.
      — done e7c1f26, 8e07c77 and 9c86b06, as the Phase 4 brief decided
      (permissions editing does not exist on Windows; the rows are hidden
      where the platform has none): `platform::POSIX_PERMISSIONS` is `false`
      on Windows, the spot panel has no Owner or Permissions row for a local
      file there (no Read-only chip), no octal in its title and no bit keys in
      its hint, the Permissions linemode says `rw`/`ro` and ` h` for a hidden
      file, Owner says `—`, and `C` is refused with "Permissions can't be
      changed on this platform". A server's file keeps all of it and the card.
- [x] **W4.30** `trash://` and `sftp://` pseudo-paths compared as `Path` (`tab.rs:770–790`,
      `trashview.rs:72–78`, `app.rs:2486, 6785, 16555`): add a unit test that
      `Path::new("trash://") == PathBuf::from("trash://")` and that
      `PathBuf::from("sftp://h/a").starts_with("sftp://h")` hold on Windows (they
      should, since neither has a drive prefix and equality is component-wise), so
      the assumption is checked rather than believed. Done when: the test passes on
      the runner.
      — done f0ed81a: `pseudo_paths_compare_as_paths_everywhere`.
- [x] **W4.26** Tests: Windows twins for `crumbs`, `typed_path`, `shorten_home`,
      `save_target`, `parse_file_uri`; `#[cfg(unix)]` on the df-app tests appendix B
      §7 flags that S1.33 did not already gate. Done when: `cargo test -p df-app`
      green on the runner. Phase 3 (port/paths) did the `crumbs` twin (W4.9)
      and three fixtures: the tab tests list a temp folder of forty files, not
      `/` (a drive's root on the runner holds a handful), and the sync card
      and state-store tests read labels and keys as the platform writes them.
      What the runner still failed at the end of Phase 3 (14 of 1,117) is
      listed by cause in `03-paths.md`'s Decisions log (2026-09-29, "df-app
      on the Windows runner").
      — done e28af95 with the twins above (W4.11, W4.12, W4.13) and the
      separator fixes (W4.13, W4.14): the gvfs card test runs on Unix only,
      the 7-Zip test reads the system's own words, the sleeve test skips where
      `bash` is WSL's launcher. df-app on the runner: 1,145 of 1,145 pass at
      run 36662477949.
- [x] **W4.33** `platform::appearance` Windows body (S1.35's surface: `Desktop`,
      `Connect`, `session`): the system's light or dark for `[flavor] mode =
      "auto"`, in place of the stub that answers nothing and says `Link::Gone`.
      Two routes, to be chosen here and logged: winit 0.30.13 already reports the
      system theme on Windows (`Window::theme()`, `WindowEvent::ThemeChanged`),
      with the same catches as on macOS (M2.30: the seam asks before the window
      exists, and `App::window_theme` setting the window's theme stops
      `ThemeChanged`); or the registry value `AppsUseLightTheme` under
      `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` (`0` dark,
      `1` light) read with windows-sys `RegGetValueW`, and heard with
      `RegNotifyChangeKeyValue` on the watcher's thread, which needs no window.
      Done when: a unit test of the value → `Scheme` mapping passes on the
      runner; live check that switching Settings → Colours turns a window in
      `auto` (V7 §5.1).
      — done 6bdc883, the registry route (Decisions log).
      `the_value_is_the_side` and `the_first_answer_is_there_at_once` on the
      runner.

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

## 9. Found in Phase 1

- [x] **W4.31** `platform::process::tie_to_this_thread` Windows body (S1.50),
      in place of the no-op under which a child outlives a crashed delightfile:
      put the child in a job object created with
      `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (`CreateJobObjectW`,
      `SetInformationJobObject`, `AssignProcessToJobObject` via `windows-sys`),
      the handle held by the owning worker, so the system ends the child when
      the last handle closes — however the process died. Done when: a runner
      test ends the parent with `TerminateProcess` and sees the child gone.
      — done 3bb0ecf, 4223e26, green on the Windows runner at run 36666369775: a
      parent ended by `TerminateProcess` takes its tied child with it
- [x] **W4.32** Cloud remotes on Windows (S1.51): `rclone rcd` serves a unix
      socket, which `std` cannot connect to on Windows, so `platform::socket` is
      refused there and every cloud remote says "Cloud remotes is not available
      on this platform". Choose the transport and record it: Windows 10's own
      `AF_UNIX` through `windows-sys` (`socket`, `connect`, `send`, `recv`, with
      the socket directory made private by an ACL rather than mode 0700), or
      `--rc-addr 127.0.0.1:0` with rclone's `--rc-user`/`--rc-pass` and a random
      password, since a loopback port is open to every local user. Then give
      `platform/windows/socket.rs` that body and `AVAILABLE = true`. Done when:
      `vfs/rclone_tests.rs` runs on the Windows runner.
      — blocked: the transport is a choice between two designs with different
      security models, which the task leaves open and the df-core brief did
      not make; the options and what each costs are in Open questions. The
      daemon's job object (W4.31) is in place for either.
      — done 09a2d88, green on the Windows runner at run 36737465177,
      Windows' own `AF_UNIX` as Brian decided, step by step: (a) run
      36724365335 installed rclone 1.75.1 and a .NET client got `HTTP/1.1
      200 OK` and `{"probe": "W4.32"}` from `rc/noop` on
      `unix://D:\a\_temp\rclone-socket\probe.sock`; (b)
      `platform/windows/socket.rs`, WinSock through `windows-sys`, the socket
      in `%LOCALAPPDATA%\delightfile\run`; (c) `vfs/rclone_tests.rs` and
      `vfs/http.rs`'s socket tests run there (Decisions log). Two fixes the
      runner asked for on the way: a `select` before each `recv` and `send`,
      since `SO_RCVTIMEO` did not end an `AF_UNIX` read, and
      `platform::fs::sync_file`, since `FlushFileBuffers` refuses a handle
      opened only to read.
- [~] **W4.34** Take `-A dead_code` off the windows job's clippy line in
      `.github/workflows/ci.yml` (S1.53). It is there because df-app items
      whose only callers are Linux bodies — the drag-and-drop helpers in
      `dnd.rs`, gio's output readers in `mounts.rs`, `platform/icon.rs`,
      `platform::desktop::Event`'s drag and clipboard variants and others the
      Linux device alone constructs — are dead on Windows until this phase
      gives them callers (W4.16–W4.19). `platform/icon.rs` is the drag image,
      whose Windows caller is the deferred W4.27; while that stays skipped the
      icon has no caller here and this task cannot close. Done when: the
      callers land and the windows job's clippy passes without the flag.
      — skipped in part, as Brian decided for macOS in M2.31 (Decisions log):
      after M2.31's moves the Windows build has 25 dead items and df-core
      none. Six are the enum variants only Linux's bodies make, the ones M2.31
      names (`ClipError::Missing`, `mounts::Reply::Mounted`,
      `mounts::Connected::NeedsTerminal`, `desktop::PasteFailure::{Stalled,
      Broken}`, `appearance::Link::Starting`, gio's `mounts::Change`s), which
      Windows says its own way too: the clipboard is always there, Windows
      mounts disks itself and Explorer asks for a share's password, the
      clipboard is synchronous, the registry answers before `watch_over`
      returns, and there is no drive watcher. The rest wait on the skipped
      W4.27: `platform/icon.rs` (the drag picture: `Icon`, `draw`, `Canvas`,
      its constants and digits) and `desktop::Event::DragEnded`. The flag
      stays; b71c6ee says why in `ci.yml`.

## 10. Found in Phase 3

- [x] **W4.35** `platform::meta::blocks_bytes` on Windows, split from W4.5
      when P3.4 did its identity half: the allocated size by
      `GetCompressedFileSizeW` (compressed and sparse files report what they
      occupy, as `st_blocks` does), in place of the file size. It needs the
      path, which a `Metadata` does not carry, so it becomes
      `blocks_bytes(path, &meta)` (Unix: off the `stat`, the path unread) and
      the du walk's `sizes_of` passes the entry's path. Done when: a du test on
      the runner sees a sparse file's total below its length.
      — done b56cea9, green on the Windows runner at run 36666369775: a sparse file
      of eight megabytes totals less than its length
- [x] **W4.36** `platform::time::local_civil` before 1970 on Windows: the
      CRT's `localtime_s` refuses a negative `time_t`, so a file dated before
      the epoch has no civil date and the bulk card's date tokens say so. Use
      `FileTimeToSystemTime` + `SystemTimeToTzSpecificLocalTime` (windows-sys,
      `Win32_System_Time`) from the `FILETIME` the seconds make, which covers
      1601 onwards. Done when: `rename::facts::tests::a_time_before_the_epoch_is_still_a_date`
      loses its `#[cfg(unix)]` and passes on the runner.
      — done 33d49f1, green on the Windows runner at run 36666369775:
      `rename::facts::tests::a_time_before_the_epoch_is_still_a_date` runs there
- [x] **W4.37** `ops::delete::remove_tree`'s "already gone" on Windows: a file
      another deleter has marked for deletion answers `ERROR_ACCESS_DENIED`
      (`STATUS_DELETE_PENDING` underneath) to the next open or delete, where
      Unix answers `ENOENT`, so two deletes of one tree race to a failure.
      Read it as gone — through a `platform::errno::is_delete_pending`, or by
      treating an access-denied `lstat` of a name the directory no longer
      lists as gone — and let `gone` ask it. Done when:
      `ops::delete::tests::a_tree_emptied_by_somebody_else_meanwhile_is_not_a_failure`
      loses its `#[cfg(unix)]` and passes on the runner.
      — done ebff427, 7d8b776, 3143ad8, green on the Windows runner at run
      36666369775: the race test runs there

## 11. Found in Phase 4

- [x] **W4.38** df-app's console tools through `df_core::platform::process::quiet`
      (W4.2's df-app half, split off because the df-core branch keeps out of
      df-app): the 7-Zip and tar listings in `preview/listing.rs` (`:324`,
      `:379`, `:930`) and any other console program df-app starts headlessly,
      so none flashes a console window on Windows. Never an opener or a new
      window of delightfile itself. Done when: each such `Command` goes
      through `quiet` (grep), and the listing tests pass on the runner.
      — done d43e913, green on the Windows runner at run 36737465177:
      7-Zip's two listings, the listing tests' fixture maker, and fd and rg
      behind the search panel (Decisions log). Not seen on the VM (07
      §5.8).

## 12. Native feel

- [>] **W4.39** The window's own top row in the title bar (Brian, 2026-09-30):
      one header where there were two — the system's caption over the app's
      top row — as Explorer, Terminal and Edge have one. **The native frame
      stays**: rounded corners, shadow and resize edges are Windows'; the
      client area is extended over the caption and the window says which
      parts of the band are title bar (drag, double click to maximize, right
      click for the system menu) and which are its own. Not
      `with_decorations(false)`. **The caption buttons are the window's**
      (Brian, 2026-09-30, Open questions resolved below), drawn as Terminal,
      Chrome and Zed draw theirs, and reported to the system as caption
      buttons so Snap Layouts still comes up under Maximize.
      - Mechanism (`platform/windows/titlebar.rs`): after winit makes the
        window, `window::adopt` puts a window procedure in front of winit's
        (`SetWindowLongPtrW(GWLP_WNDPROC)`, winit's put back at
        `WM_NCDESTROY`), extends the frame one pixel into the client area
        (`DwmExtendFrameIntoClientArea`) and has the frame measured again.
        `WM_NCCALCSIZE` takes the default frame and gives its top back, a
        maximized window's top brought in by the frame's depth
        (`SM_CYSIZEFRAME` + `SM_CXPADDEDBORDER`) so nothing hangs off the
        screen; `WM_NCHITTEST` takes the default (the side and bottom
        borders) and classifies what it calls client (`platform/caption.rs`,
        tested on every target): `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE` over
        the three buttons, `HTTOP`/`HTTOPLEFT`/`HTTOPRIGHT` on the top edge
        unless maximized, `HTCLIENT` on the chrome's controls and below the
        band, `HTCAPTION` on the rest of the band. A window with no
        `WS_CAPTION` (winit's full screen; the app has none) is passed
        straight through, with no band.
      - The buttons, as Terminal handles its own: `WM_NCMOUSEMOVE` over one
        lights it and asks `TrackMouseEvent(TME_LEAVE | TME_NONCLIENT)` for
        the `WM_NCMOUSELEAVE` that puts it out; `WM_NCLBUTTONDOWN` (and its
        double click) on one presses it and `WM_NCLBUTTONUP` on the same one
        posts the `WM_SYSCOMMAND` the system's button would — `SC_MINIMIZE`,
        `SC_MAXIMIZE` or `SC_RESTORE`, `SC_CLOSE` — both kept from the
        default procedure, which would track buttons of its own metrics;
        moves go on to it, which is where Snap Layouts comes from.
        `caption::track` is the reading, tested on every target; a change
        asks for a frame (`RedrawWindow(RDW_INTERNALPAINT)`), which reads
        `window::caption_pointer`. `chrome::caption_buttons` paints them at
        Windows 11's geometry — three 46-point buttons, the band's full
        depth, at the window's right edge — with the glyphs' shapes drawn as
        strokes (a line; a square, or restore's two while maximized; an ×)
        in the text's colour at `glyphs::weight`; under the pointer
        minimize and maximize on the chips' hover plate (`surface1`) and
        close on `#C42B1C` with a white glyph; held, `surface2` and a darker
        red. They are painted last, over everything the window shows.
      - The band (`ui::TitleBand`, `ui::layout`'s new argument): with one
        tab the top row is in it, with two or more the strip, the top row
        below as in Explorer; the row a `GAP` short of the three buttons
        (`TitleBand::buttons`, `Layout::caption`). After each layout the app
        reports the buttons and the controls in the band
        (`chrome::band_controls`: ☰, crumbs and `…`, the filter chip, the
        cluster's chips and counter, the tab chips and `+`, or a prompt's
        field) through `window::title_regions`, which the hit test reads.
      - `window::set_theme` sets `DWMWA_USE_IMMERSIVE_DARK_MODE` for DWM's
        frame beside winit's theme; the buttons follow the palette, being
        painted by the window.
      - Linux answers no band, and lays out as it did: a test holds five
        of `main`'s layouts at 39c4d51 to the point. (macOS answered none
        too until `02-macos.md` M2.37 gave it the traffic lights' band.)
      Done when: the runner is green, and the VM shows the §5.1 checks of
      `07-verification.md` marked W4.39.
      — titlebar agent (`port/titlebar`), started 2026-09-30; e40bc6b,
      04a3425, then the window's own buttons in 77f803f and f2cd5c8, green on
      the Windows runner at run 36731513722 (df-app 1,163 there, the caption
      tests among them). On the VM (07 §5.8) the window has one header, its
      top row at the top of the window and the drawn buttons at its right
      end; the viewer failed before the rest of §5.1's W4.39 lines could be
      tried, so they are still to see.

## 13. First hands-on round, 2026-10-01

Brian's first session with the v0.1.0 zip on Windows found seven things that
read as a Linux program on a Windows desk. The decisions are his (2026-10-01);
the calls the brief left to the builder are in the Decisions log, marked
`(polish)`. Branch `port/windows-polish`, from `main` at 849a416.

- [x] **W4.40** The `g` chords are a Windows user's places: `g h` the profile
      folder, named Home; `g d`, `g D`, `g o`, `g p`, `g v`, `g m` Downloads,
      Desktop, Documents, Pictures, Videos and Music where
      `SHGetKnownFolderPath` says they are (a Documents OneDrive took is found
      in `OneDrive`), each else its folder under the profile; `g c` the
      system drive's root (`%SystemDrive%\`, `C:\` when unset); `g a`
      `%APPDATA%`, named AppData, where the config lives; `g /` the Places
      card (`KEYMAP_OVERRIDES`, the mount-manager command), a Windows machine
      having no root. `g w` and Brian's machines are gone. Each is written as
      Windows writes it and said so on the which-key card ("Downloads ·
      C:\Users\admin\Downloads") and the Places card (name, then path); a
      place under home is shown whole on Windows wherever a list shows one
      (`platform::os::HOME_AS_TILDE`). The seam's table is
      `platform::defaults::bookmarks()`, and a `config::Bookmark` may carry
      the name the Places card shows. Linux's table and lists are unchanged.
      Done when: the runner pins the keys and paths and `g /`, and the VM
      shows the which-key card and the Places card.
      — done 33cf363 (with cbc9c25's test fixes), green on the runners at run
      36905030633; the runner's known folders all answer from the shell
      (`known::tests`). VM: §5.8.
- [x] **W4.41** Enter does what a double-click in Explorer does: every file's
      first opener is `builtin:shell-open`, text and archives included, and
      `O` offers the alternatives this machine has and ends every file's list
      in Windows' own "Open with…" (`builtin:shell-open-with`,
      `SHOpenWithDialog`, the chooser Explorer's "Choose another app" shows;
      `rundll32 shell32.dll,OpenAs_RunDLL` and then the shell's `openas`
      verb showed nothing on the VM).
      The table is 05 §2.2's Windows rows; a row none of whose programs is
      here is not shipped, so `O` shows no dead choice, and a program
      registered in `App Paths` rather than on `PATH` (Notepad++, VLC, 7-Zip)
      is found and run by its registered path. Zed, the Zed workspace, Chrome
      and the image editor are gone from Windows' table. Done when: the
      runner pins the principle and the VM opens a file each way.
      — done 73423cc, 63b0a90 and b2989b8, green at run 36917221800. VM:
      §5.8.
- [x] **W4.42** Without a Nerd Font the icon column is drawn, not
      classified (`marks.rs`, `icons::paint`): a folder with a tab, a page
      with its corner folded, the page with an arrow for a link and with `▶`
      for something that runs, in the kinds' colours; the Places card's disk,
      stick, phone, camera, server and plus, and the archive header's mark
      and a locked member's padlock, which were blank or a key emoji. Each is
      the Nerd Font picture's size and place, on whole pixels, scaling with
      the font. With a Nerd Font the glyph is drawn exactly as before
      (`with_a_nerd_font_the_icon_column_is_unchanged`, run here with
      JetBrainsMono Nerd Font installed). Done when: the VM shows the column,
      the grid and the Places card with no `/` and no blank.
      — done e02d7c7, green at run 36905030633. VM: §5.8.
- [x] **W4.43** Enter on a font (`*.{ttf,otf,ttc}`) installs it for this
      user: `builtin:font-install`, the shell's `install` verb through
      `ShellExecuteExW`, found in the item's shortcut menu as Explorer's is
      (`SEE_MASK_INVOKEIDLIST`) and seen through on a task worker that keeps
      its apartment and pumps its messages, toasting "Installed <family
      style>" once the font is in a fonts folder, and failing the task with
      why when it is not; where the verb is missing, the Font Viewer opens
      instead and the toast says so.
      `O` offers "Preview in Windows Font Viewer" and "Open with…". Done
      when: the verb is seen on a Windows that has it, and the VM installs a
      test font.
      — done e4a11d6, 0c93a72 and 6478f43, green at run 36914607187. The
      runner's Server image has no `.ttf` association at all (neither `open`
      nor `install`), so its tests say so and skip. On the Windows 11 VM the
      first two installed nothing, and 6478f43's put Hack Regular in
      `%LOCALAPPDATA%\Microsoft\Windows\Fonts` and said "Installed Hack
      Regular" (§5.8).
- [x] **W4.44** An SVG previews on Windows and macOS: their FFmpeg has no
      librsvg (BtbN's `--enable-*` list at `build/windows/ffmpeg.lock` has
      none, where Arch's has `--enable-librsvg`), so `decode` fell through
      to FFmpeg's `svg_pipe` with no decoder and said "no decoder".
      `platform::svg::render` draws one with resvg 0.48 before FFmpeg is
      tried, fitted to the pane, its words in the faces of
      `platform::fonts::dirs`; on Linux it answers nothing and FFmpeg's
      librsvg draws it as before. resvg is built off Linux only. Done when:
      the runner draws an SVG with a path, a gradient and text.
      — done bdfc829, green at run 36905030633
      (`platform::svg::tests::a_drawing_with_a_path_a_gradient_and_text_is_a_picture`
      on Windows and macOS). VM: §5.8.
- [x] **W4.45** An Affinity document previews as the thumbnail it carries,
      on every platform (Brian asked for Linux too): `df_core::preview::
      affinity` follows the header to the `Thmb` record Affinity writes in
      every save, and where there is none to follow scans the first 16 MiB
      for the record and then for the largest well-formed PNG; the sniff
      knows the signature (`00 FF 4B 41`) and the extensions name it
      (`application/x-affinity`), so its kind is `PreviewKind::Affinity`,
      decoded in df-app (`Full::Embedded`) with an "embedded preview" chip,
      and a file with none is a card — its name, "Affinity Designer
      document", its size and dates — where it was a hexdump. Done when:
      unit tests over a synthetic file, and Brian's own `.af` and
      `.afdesign` files.
      — done 20ea4f3, green at run 36905030633. All seven of Brian's files in
      `~/Windows` (Designer 1's `.afdesign`, Affinity 3's `.af`) gave their
      thumbnail by the header's pointer. VM: §5.8. PSD's embedded thumbnail
      is a follow-up, not done.
- [x] **W4.46** A place a bookmark, a pin or `Go to:` names is written with
      `\` alone on Windows (`df_core::path::native`): `g D` goes to
      `C:\Users\admin\Desktop`, not `C:\Users\admin/Desktop`; `Go to:` reads
      `C:/x`, `//server/share` and a mixed path the same way; a `[goto]`
      row's which-key line says its path so. Linux and macOS keep the text as
      typed. Done when: tests for `~/Desktop`, `C:/Users/x`, `//server/share`
      and a mixed path, and the VM's breadcrumb after `g D`.
      — done 6e4eac7, green at run 36905030633. VM: §5.8.

## 14. Follow-ups to the first round, 2026-10-01

Two things the first round's VM run left open. Branch `port/windows-repaint`,
from `main` at 6fd5c2a.

- [x] **W4.47** Whether a frame no input brings reaches the screen on
      Windows: the first round saw toasts only while keys or the pointer
      drove frames (the Open question it left). **It does; nothing was
      wrong.** A build of 6fd5c2a's code with a frame-log probe (d62bd26,
      not kept; its present mode and its loop as `main` has them) was run on
      the VM with the log in a file: a toast rose, rested and faded on the
      loop's own deadlines with no input, and on screen a `Created file`
      toast was up after the key and gone by itself, a copy's bar went from
      177 MB to 493 MB to done and its undo toast came and went with nothing
      touched, and a row for a file made from the host appeared with no input
      at all. Once everything had settled the window drew no frame for the
      23 s until it was closed. What the first round saw was the viewer: the
      browser tool's actions reach the VM seconds apart (a key and the next
      pointer move 5.1 s apart in the log), a notice lives 2.2 s, and so a
      screenshot after a key mostly came after the toast had gone. Done
      when: the log shows the deadlines firing with no input, and the VM
      shows a toast come and go, a bar move and a row appear untouched.
      — checked, no change; the probe and a Fifo build were measured and
      dropped (Decisions log). VM: §5.8.

## Decisions log

- 2026-09-25 — No hand-written COM; drag-out and Recycle Bin restore deferred.
- 2026-09-25 — Argv openers with `$1`/`$@`/`$dir`; `builtin:shell-open`.
- 2026-09-25 — Thread-per-pipe SFTP transport on Windows; Unix keeps `poll`.
- 2026-09-25 — WARP fallback adapter allowed on Windows only.
- (df-app) 2026-09-29 — W4.33 appended: light mode's `auto` (which arrived after
  this plan) reads the XDG portal on Linux; on Windows S1.35 leaves a stub that
  answers nothing, and W4.33 is its native body.
- 2026-09-29 — W4.9 and W4.10 were done in Phase 3 (port/paths): the
  brief for Phase 3 named the breadcrumb's segments and the displayed root as
  the df-app sites the path model itself requires, and a drive root's lack of
  a parent pane needed nothing (`Path::parent` of `C:\` is `None`, as of `/`).
  W4.11–W4.14, the rest of df-app's paths, stay here.
- 2026-09-29 — W4.5's file identity (volume serial, file index, link count)
  was done in Phase 3 under P3.4, with P3.3's `same_file`, because the
  Windows runner's df-core suite could not go green without it; its
  allocated-size half is W4.35. `windows-sys` 0.52.0 (winit's) is df-core's
  from that commit, features `Win32_Foundation` and `Win32_Storage_FileSystem`.
- 2026-09-29 — The appearance body was appended as `W4.31` on the df-app branch
  while the df-core branch took `W4.31` for the job object; at integration the
  job object kept it and the appearance body became `W4.33`
  (`01-platform-seam.md` Decisions log).
- (df-app) 2026-09-29 — The keyboard report from the VM (main at 490b481: `j`,
  `k`, `h` and ↓ did not move the cursor) has no cause in the code: winit
  0.30.13 hands Windows' ↓ over as `key_without_modifiers` `ArrowDown` with no
  text, the keypad's ↓ with Num Lock on the same, and a letter as its
  character with its text, and df-app's key path at 490b481 is the one it has
  now. On the VM, the build of 3b368bd (run 36653756431, 07 §5.8) moved the
  cursor with ↓ and ↑, opened the find prompt with `/` and wrapped at the
  bottom, and `j`, `k`, `h` moved nothing — which they do nowhere, the list's
  cursor keys being the arrows and `j` `k` `l` the transport's
  (`keymap::RESERVED_TRANSPORT_KEYS`). What is left is the ↓ of that report,
  which this build does not reproduce; the likeliest reading is the keyboard
  left with the console window the console-subsystem build opened beside the
  window, which W4.1 removes. 821f346 makes the path testable (`keys::Stroke`,
  `App::key_down`, `keystrokes_shaped_as_windows_sends_them_reach_the_cursor`)
  and traces every key down at `RUST_LOG=trace`.
- (df-app) 2026-09-29 — A window that would not fit on the screen opens cut to
  fit (e2b564e): winit reports no work area on any platform, so Windows asks
  `SystemParametersInfoW(SPI_GETWORKAREA)` for the primary monitor's and
  `AdjustWindowRectExForDpi` for the frame, and centres the fitted window
  there; macOS cuts the size to `NSScreen.mainScreen.visibleFrame` less the
  title bar and leaves the placing to AppKit. Linux is unchanged: a Wayland
  client is told no screen's size, and X11's answer is not asked (the pure
  rule, `platform/fit.rs`, is tested everywhere).
- (df-app) 2026-09-29 — W4.3: a typed `;`/`:` line and an opener are run by
  two contract functions each (`spawn_detached`/`run_blocking`,
  `spawn_typed`/`run_typed`), the Unix bodies of the second pair calling the
  first, because on Windows one is an argument list and the other `cmd`'s.
  `builtin:shell-open` in a Linux or macOS config says "`builtin:shell-open`
  is not something delightfile can do", the words any unknown builtin had
  before.
- (df-app) 2026-09-29 — W4.3, from the VM: a typed line runs under
  `CREATE_NO_WINDOW`, as a Linux one runs with no terminal, because `;notepad`
  opened a `cmd` console beside Notepad that stayed while Notepad ran; what
  the line starts still shows its own window. In a share's folder the line
  runs between `(pushd "<folder>" || exit 1)` and `popd` (d3bf75c, 4d56ecf),
  since `cmd` refuses a UNC working directory and starts in the Windows
  folder, where a relative name in the line would act. `pushd` maps the share
  to a free letter, which stays mapped for the session unless `popd` gives it
  back, so the tail gives it back and exits with the line's own status, read
  through `call` after the line has run (`typed` in `platform/windows/open.rs`
  says how); a runner test sees the status and no new letter, and on the VM
  the Places card listed the letter while Notepad ran and not after (07 §5.8).
  An opener is not changed: it is its program, started directly.
- (df-app) 2026-09-29 — W4.12 is P3.31's option (a): the bulk card's one list
  of a row's problems asks `path::name_is_valid`. On Linux the one change is a
  NUL in a row or in `Save as:`, now refused in `name_is_valid`'s words; no
  key types one.
- (df-app) 2026-09-29 — W4.13: on Windows `parse_file_uri` reads a remote
  authority as the share it names (`file://server/share/x` is
  `\\server\share\x`), where Unix refuses it, because a Windows program writes
  a share that way and Windows opens it like any other path. The task's
  `paths_from` row is left to Linux: M2.31 moved it into the Wayland device,
  which is the only reader of a plain-text drop.
- (df-app) 2026-09-29 — W4.16 converts where the task said PNG only: a picture
  that is not a PNG is made one for a copy (`image` decodes every format the
  window copies by value), and a `CF_DIBV5`/`CF_DIB` another program offered
  is made one for a paste (a bare Print Screen, an old program). The system
  clipboard is copied to by `Y` and read by `p` with nothing yanked; `y` and
  `x` are the window's own clipboard, as on Linux, so `Preferred DropEffect`
  is always copy and 07 §5.5's "`x` then paste in Explorer moves" has nothing
  to check (the line is corrected there).
- (df-app) 2026-09-29 — W4.19: `mounts::CONNECT_UNSEEN` is "Explorer was asked
  to connect to" on Windows, where main had `None` while nothing connected
  there. A connect whose share has not answered in its 10 s may still be
  behind Explorer's password dialog, which is M2.15's case on macOS, so the
  window says as much as it knows.
- (df-app) 2026-09-29 — W4.33 takes the registry route: the question goes out
  before the window exists, and a window whose theme `App::window_theme` sets
  stops reporting the system's, so winit's `ThemeChanged` would have needed
  the same work-arounds as macOS's M2.30; `AppsUseLightTheme` and
  `RegNotifyChangeKeyValue` need no window.
- (df-app) 2026-09-29 — W4.29 follows the Phase 4 brief rather than the task's
  Read-only chip: the spot panel has no Owner or Permissions row for a file on
  a Windows drive, since permissions editing does not exist there (decided for
  Brian, who delegated it; the df-core branch logs it with the nofollow stub,
  f320cb7 on `port/windows-core`). With the rows go the octal in the panel's
  title and the permission-bit keys in its hint, which the VM showed left
  behind (9c86b06); Space there is hash alone. The Permissions linemode has no
  `s` for a system file: `Entry` carries no system attribute.
- (df-app) 2026-09-29 — W4.8: the undo record of a Windows trash stays on the
  stack — the journal has no way to drop the top record, and dropping it would
  hide that the trash happened — so `u` keeps saying where to restore it until
  something else is done on top.
- (df-app) 2026-09-29 — W4.34 skipped in part: Brian's M2.31 decision for
  macOS — the Linux-only functions moved under `platform/linux/`, the six
  Linux-only enum variants kept with `-A dead_code` — covers the same six on
  Windows, and the rest of what is dead there is the drag picture and
  `Event::DragEnded`, whose caller is the skipped W4.27. The windows job keeps
  the flag until W4.27 is done.
- (df-core) 2026-09-29 — W4.7: a path is recycled only on a fixed drive
  (`GetDriveTypeW` of its volume is `DRIVE_FIXED`), not on a fixed *or
  removable* one as the task said. Windows keeps no Recycle Bin on removable
  media, and there `SHFileOperationW` under `FOF_NOCONFIRMATION` deletes for
  good without a word; refusing (`"<volume> has no Recycle Bin"`) sends `d` to
  the permanent-delete confirm, which is what the task asks for wherever there
  is no bin. `Trash::trash` asks it too, so a redo through `Trash::at` cannot
  reach such a drive either.
- (df-core) 2026-09-29 — W4.7: a verbatim path (`\\?\C:\…`, `\\?\UNC\…`) is
  handed to the shell in its plain spelling, which names the same file; one
  whose plain spelling would name another (a `.` or `..` name, a name ending
  in a dot or a space, a `/` inside a name) or that has none (a volume GUID, a
  device) is refused before the call, as the too-long path is.
- (df-core) 2026-09-29 — W4.7: the Windows trash has its own `TrashedItem`,
  `Purged` and date helpers (the stub's, copied), and `platform/stub/trash.rs`
  is gone: after the rebase onto Phase 2, which gave macOS its own trash,
  nothing used it. Reason for the copy: Windows' item is a real one whose
  location is unknown rather than a stand-in.
- (df-core) 2026-09-29 — Permissions editing (the `C` card, `ops::mode`) does
  not exist on Windows; decided for Brian, who delegated the open question
  that asked it. Windows has one permission bit, read-only, which W4.29 shows
  as the spot panel's chip through `platform::fs::apply_mode`, and ACLs the
  nine-bit grid cannot show. So `platform::nofollow` stays the stub there:
  `ready()` refuses with `Unsupported("Permissions")` and the chmod walk is
  never reached. How df-app presents the absence (the command's refusal, the
  menu) is the df-app branch's.
- (df-core) 2026-09-29 — W4.35: `blocks_bytes` takes the metadata and a
  closure that builds the path, `blocks_bytes(&meta, || path)`, rather than
  the path itself as the task wrote. Reason: the du walk has no path for a
  file until it builds one (`DirEntry::path` allocates), and building it for
  every file would be a cost on Linux for an argument Unix never reads; with
  the closure only Windows builds it, and only for a regular file.
- (df-core) 2026-09-29 — W4.37: `platform::errno::is_delete_pending` reads
  the NT status under an access-denied error (`RtlGetLastNtStatus`, ntdll's,
  declared by hand), the first of the task's two routes. The second — an
  access-denied `lstat` of a name the directory no longer lists — cannot
  tell: a delete-pending name is still listed until its last handle closes.
  The status is the thread's record of its last failure, so `gone` is asked
  straight after each failing call, as `remove_tree` already did; it also
  reads a not-found status under an access-denied error as gone, which is
  what `std`'s `symlink_metadata` returns when its directory lookup finds the
  name gone after a denied open. On Linux `is_delete_pending` is `false` and
  `gone` is the `NotFound` check it was; the top `lstat` of `remove_tree` now
  asks `gone` too, the same check there.
- (df-core) 2026-09-29 — W4.2: `quiet(&mut Command) -> &mut Command` sets
  `CREATE_NO_WINDOW` on Windows and nothing elsewhere, and returns the
  command so a builder chain takes it in place. Every console tool df-core
  starts goes through it: `git status`, the decompressors (listing and
  unpacking a compressed tar), the compressors behind `A`, 7-Zip and bsdtar
  (extracting, `A` to 7z), the version probe that greys a format out,
  `rclone`, and — with W4.21 — `ssh` and a `program` service. Not `rsync`,
  which is never started on Windows (`HAS_RSYNC`). df-app's own are W4.38.
  The test checks the flag by value and that a quiet tool's pipes still
  carry its output: `std` has no getter for a command's creation flags, and
  a console that never opens cannot be seen headlessly.
- (df-core) 2026-09-29 — W4.2: `CREATE_NEW_PROCESS_GROUP` is not df-core's.
  It detaches an opener from the app (`00-ground-rules.md` §5), df-core
  starts no opener, and df-app's `platform::open` is where openers are
  detached (macOS's `detach` is there), so it is W4.3's. `NULL_DEVICE`,
  `is_executable`, `candidates`, `exit_code`, `pause`/`resume` and
  `terminate` (`Child::kill`, `TerminateProcess`) were written in Phase 1.
- (df-core) 2026-09-29 — W4.6: `symlink` keeps its `io::Result`, so the
  refusal without the privilege is an `io::Error` (`PermissionDenied`,
  "Creating links needs Developer Mode or an elevated process") rather than
  the task's `DfError::Op`; every caller wraps it as `DfError::io(link, e)`,
  so the person reads the link's path and those words. Changing the
  signature would have changed every caller on Linux for the same sentence.
  `set_times` on the reparse point and `apply_mode` as the read-only
  attribute were written in Phase 1; this task gave them tests.
- (df-core) 2026-09-29 — W4.6: the `writable` probe is opened with
  `FILE_FLAG_DELETE_ON_CLOSE`, hidden and temporary, rather than created and
  then deleted by a second call: the system removes it when the handle
  closes, so a crash between the two leaves nothing behind. Its only caller
  today is Linux's trash (`available_for`); on Windows nothing asks it yet.
- (df-core) 2026-09-29 — W4.4: besides the re-arm the task names, a watched
  directory is checked by name at each flush, and one no longer there (or no
  longer a directory) is `Gone`. Reason: a handle does not notice its
  directory renamed away, and a directory deleted with POSIX semantics
  leaves the name at once while the handle's read may never complete; the
  name is what the pane shows. A read that completes with an error other
  than `ERROR_NOTIFY_ENUM_DIR` is `Gone` too (a deleted directory's pending
  read fails with access denied). Every watch is looked at after any wake,
  since `WaitForMultipleObjects` reports only the lowest signalled handle,
  and at most 63 directories are watched, its limit less the wake event.
- (df-core) 2026-09-29 — W4.4: `platform/stub/watch.rs` is gone, as the trash
  stub is (W4.7): macOS has kqueue since Phase 2.
- (df-core) 2026-09-29 — W4.31: the seam gains `tie(&Child) -> io::Result<Tie>`
  beside `tie_to_this_thread(&mut Command)`. Reason: a process goes into a
  job only once it exists, and `std` has no stable way to start one inside a
  job (the attribute list is unstable), so Windows' half comes after the
  spawn and its handle needs an owner — the `Tie`, which the rclone
  `Daemon` holds for its life. On Linux and macOS `tie` does nothing and the
  `Tie` is empty; Linux's parent-death signal is unchanged. A delightfile
  killed between the spawn and the assignment leaves that one child running.
  The runner test ends a parent with `TerminateProcess` and sees its tied
  child end; nothing on Windows spawns the daemon until W4.32.
- (df-core) 2026-09-29 — W4.31: `platform/stub/process.rs` is gone, as the
  trash and watch stubs are: macOS has its own parent-death watcher since
  Phase 2.
- (df-core) 2026-09-29 — W4.20: stdin has a thread too, `df-sftp-in`, where
  the task kept it synchronous with a watchdog check on the next call. A
  synchronous write the child has stopped draining never returns, so no
  check after it runs; with the thread, a write waits for its answer with
  the time left and times out as a read does (connection-fatal, so the
  transport kills the child, which breaks the pipe under the thread and
  frees it). None of the three threads is joined: each ends at its pipe's
  end or when nobody listens, and waiting could hang on a grandchild that
  inherited a pipe.
- (df-core) 2026-09-29 — W4.20: the seam is `platform::pipe::Pipes` —
  `open`, `read`, `write`, `drain_stderr`, `stderr` — rather than the task's
  `Reader` with `read_until`, because writes needed a home too. On Unix it
  is the `poll` code `vfs::conn` had, moved behind it: the same calls in the
  same order, the same errors for the same outcomes (`fill` and `write_all`
  keep their loops and their deadline checks; the wait-and-call step inside
  each is the platform's). `pipe::AVAILABLE` is gone, every target having
  pipes now, and with it the refusal before `ssh` starts.
- (df-core) 2026-09-29 — W4.20: the replay binary is the example
  `sftp_replay` (`crates/df-core/tests/bin/replay.rs`), not a `[[bin]]`:
  `cargo test` builds examples before it runs tests, and a unit test finds
  one beside its own binary, where a `[[bin]]`'s path is given only to
  integration tests. A `--lib` run has no examples, and the Windows tests
  that need it skip, saying so — but on a runner (`CI` set), where a
  missing example means a broken build, they fail. The six fake-server
  tests lost their `cfg(unix)`; Unix keeps `/bin/sh` for them. The
  reconnect test needs OpenSSH's `sftp-server`, which the runner's Windows
  has not, so it has a Windows twin of the same name: a replay server that
  answers the handshake and one `STAT`, ended by `taskkill` through the pid
  file it writes.
- (df-core) 2026-09-29 — W4.21: `ssh` is not resolved by hand through
  `candidates`. `std`'s Windows lookup adds `.exe` to a bare name and
  searches `PATH` (after the program's own directory and the system's),
  finding the `ssh.exe` that `candidates("ssh")` names first, and Windows'
  OpenSSH is on `PATH` as it ships; resolving it here would also have put a
  full path into Linux's argv and error text for nothing. `quiet` is applied
  to `ssh` and to a `program` service. The key's `~` was already expanded
  through `platform::dirs::home` (`path::expand_home`), and the download
  temp names made valid in Phase 3 (P3.21). A test runs the real `ssh`
  against a closed port wherever there is one and reads its refusal off
  stderr through the new pipes.
- (df-core) 2026-09-29 — W4.37, from the runner: after access denied, the
  race's last failure was "not empty" from the folder both deleters had
  emptied — its last names marked by the other and not yet let go, since a
  marked name stays listed until its last handle closes. `remove_tree` tries
  the folder again, every 5 ms for a second at most, while
  `platform::fs::is_emptying` says every name left is delete-pending; on Unix
  that is always `false`, so the first "not empty" is the error it was. A
  gone error from the listing itself now stops the listing rather than
  asking again (Windows answers every later call the same; on Unix the
  listing had already ended), and `STATUS_FILE_DELETED` — a call on a folder
  deleted since it was opened — reads as gone too.
- (df-core) 2026-09-29 — W4.7 turned one df-app test red on Windows:
  `app::tests::the_trash_is_refused_where_the_platform_has_none`, under
  `cfg(windows)` "until W4.7", expected `d`, the trash's door and "Empty
  trash" to refuse with "Trash is not available on this platform". With the
  Recycle Bin every platform has a trash, so the test is now
  `the_trash_is_there_on_every_platform`, which runs on all three and holds
  that none of the three is refused that way. The df-app edit is this
  branch's, being the few lines W4.7's landing needs (allowed for W4.8's
  sake); the gate in `App::refusal` it tested is left as it is, and never
  refuses now.
- (df-core) 2026-09-29 — W4.8, "Empty trash" on Windows: option (b) of the
  question the df-app branch asked, decided for Brian and relayed to this
  branch, which takes the question out of Open questions. df-core gives
  every target `platform::trash::{BinSize, bin_size, empty_bin}`: on Windows
  the bins of every drive counted by `SHQueryRecycleBinW` (items and bytes)
  and emptied by `SHEmptyRecycleBinW` with `SHERB_NOCONFIRMATION` — the
  app's own card having asked — a bin already empty being `Ok` without the
  call; on Linux and macOS both refuse (`Unsupported("The Recycle Bin")`),
  their trash being listed and purged item by item. The card that counts
  rather than lists is df-app's.
- (df-core) 2026-09-29 — The df-core half of Phase 4, rebased onto `main` at
  0894f6e (Phase 2 and the df-app half merged; the stubs
  `platform/stub/{trash,watch,process}.rs`, which only Windows still used by
  then, are gone, Windows having its own): run 36666369775 builds df-core on
  Windows and passes its 1,080 tests, 1 ignored (`main` at 0894f6e: 1,046,
  run 36664961743), in both of the job's test steps; its clippy and the
  smoke test pass; macOS and Linux are green (Linux: df-core 1,187 and 1
  ignored, df-app 1,289, 2 and 1). df-app fails 2 of 1,145 on the Windows
  runner there, where `main` fails none: the two opener tests that pinned
  Linux's table on Windows, which D5.4 replaced and c32eaa0 answers. What is
  left of this half is W4.32 (blocked, Open questions) and D5.7's macOS
  side.
- (titlebar) 2026-09-30 — W4.39's band is the window's full width from its
  top edge to the foot of the row in it (the top row with one tab, the strip
  with more) or of buttons the system draws in it, whichever is lower. The
  margin above the row is in it — the top resize edge while the window is
  restored, title bar while it is maximized — and the gap under the row is
  not, being the window's.
- (titlebar) 2026-09-30 — The row in the band keeps a `GAP` from the caption
  buttons' left edge, as it keeps one from the window's edge: `TitleBand`'s
  `right_inset` is measured from the window's right edge to the buttons, and
  the row ends a `GAP` before that.
- (titlebar) 2026-09-30 — "The row is vertically centred" when the buttons
  are deeper than the row: centred on the band once that puts it lower than
  its usual margin from the top; until then it keeps the margin, so the row
  does not jump up as the buttons pass its depth. The panes start a `GAP`
  under the band, not under the row, so no row of theirs is ever title bar.
  Since the window draws Windows' buttons itself (below) they are the band's
  depth and Windows asks for none; the rule stands for a system that draws
  its own in the band.
- (titlebar) 2026-09-30 — The frame is extended by the caption's full depth
  (the top of `AdjustWindowRectExForDpi` for the window's style at its DPI),
  not one pixel: the extended frame is where DWM draws the caption buttons
  once the caption is client area, and every message goes to
  `DwmDefWindowProc` first, as Microsoft's custom-frame guide has it, so
  DWM answers the buttons' hover, press and `HTMAXBUTTON` itself. The
  classifier's own button answer (thirds of the reported bounds) is for a
  point DWM leaves. — Superseded the same day by Brian's call to draw the
  buttons (below): DWM draws the extended frame behind the client area, the
  window's DX12 surface is opaque, and its buttons would have been covered.
- (titlebar) 2026-09-30 — Where a caption button and the top resize edge
  overlap, the button wins: it is what is drawn there. The edge is
  `SM_CYSIZEFRAME` + `SM_CXPADDEDBORDER` deep at the window's DPI (what
  Terminal uses), and its corners are that wide.
- (titlebar) 2026-09-30 — While a prompt has the top row, only its field is
  the window's in the band; the title and the error or case mark beside it
  are title bar, being words about the field that a press on does nothing.
  The help sheet's filter has no field on the row, so the whole row is
  title bar while the sheet is up. A chip fading out is still the window's:
  a press on it does nothing rather than moving the window.
- (titlebar) 2026-09-30 — The seam is five functions,
  `window::{adopt, title_band, title_regions, caption_pointer, set_theme}`
  (`caption_pointer` came with the drawn buttons, and `title_regions` took
  their rects); `set_theme`
  replaces the app's direct `Window::set_theme` so Windows can set
  `DWMWA_USE_IMMERSIVE_DARK_MODE` with it (winit sets
  `WCA_USEDARKMODECOLORS` through `SetWindowCompositionAttribute`, not the
  documented attribute). `adopt` runs before `Gfx::new`, whose surface is
  made at the client size the band is part of. `ui::top_row_width` gives
  the prompt's second-line measure the row's width in the band.
- (titlebar) 2026-09-30 — Not handled: winit sizes a window moved to a
  screen of another DPI from the caption it thinks is there, so after such a
  move the window is a caption's depth taller than before; and a maximized
  window covers its work area exactly, so an auto-hiding taskbar may not
  come up at that edge (Terminal takes a pixel off that side). Neither is
  seen on the VM's one screen.
- (titlebar) 2026-09-30 — Brian (through the coordinator) answered W4.39's
  Open question, whether DWM's caption buttons show over the window's
  pixels, before anyone had looked: **the window draws its own**, as
  Terminal, Chrome and Zed do, not the transparent swapchain. The hit codes
  stay `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE` so Snap Layouts still comes up
  on Maximize; a button acts on release; three buttons 46 points wide, the
  band's full depth, at the window's right edge; the glyphs' shapes drawn
  as strokes in the text's colour at the chrome's symbol weight; a plate in
  the chips' hover colour for minimize and maximize, `#C42B1C` under a
  white glyph for close, a shade stronger held; the hover heard from
  `WM_NCMOUSEMOVE`/`WM_NCMOUSELEAVE` and kept where the regions are; the
  right inset their width, not `DWMWA_CAPTION_BUTTON_BOUNDS`.
- (titlebar) 2026-09-30 — With the buttons the window's, the frame is
  extended one pixel, the least that keeps DWM treating it as a window with
  a frame, and `DwmDefWindowProc` is no longer called: its answers would be
  for DWM's buttons, drawn under the window's where nobody sees them, and
  an extension the caption's depth would let those show through wherever
  the surface lags a resize.
- (titlebar) 2026-09-30 — The buttons' messages are handled as Terminal
  handles its own: a press and a release on a button are the window's
  alone (the default procedure's own tracking would use the system's
  metrics, not the drawn buttons), the click is the `WM_SYSCOMMAND` the
  system's button would post (`SC_MINIMIZE`, `SC_MAXIMIZE` or
  `SC_RESTORE`, `SC_CLOSE`), and moves go on to the default procedure,
  which raises Snap Layouts. A press is drawn only while the pointer is
  still over its button; a release over another is no click; leaving the
  title bar lets go of it, the release then landing in the window, where
  these messages do not come.
- (titlebar) 2026-09-30 — The buttons' hover is instant, not faded as the
  chips' is: the chips fade through the app's hover map, which egui's
  pointer drives, and the pointer over a caption button is the system's.
  A fade would be one more animation source in `app.rs` for a tenth of a
  second. The plates are the buttons' whole rects, square as Windows' are
  (DWM's cut of the window's corner rounds close's); held, minimize and
  maximize take `surface2` over hover's `surface1`, and close's red goes 14
  % towards black. The glyphs are 10 points across, as Windows 11's are at
  100 %, restore's squares 2 points apart. The buttons are painted after
  everything else, the help sheet's scrim and the menus included: nothing
  the window shows covers its title bar's.
- (titlebar) 2026-09-30 — Not done: Windows dims the caption glyphs of a
  window that is not the active one; these stay in the text's colour.
- (finish) 2026-09-30 — W4.7, a file too big for its drive's Recycle Bin:
  option (a) of the open question, decided for Brian, who delegated it.
  `FOF_WANTNUKEWARNING` joins the recycle's flags, so before the shell
  deletes such a file for good it asks in its own dialog, owned by no window
  of ours, and a No aborts the call, which `Trash::trash` refuses as
  cancelled ("moving it to the Recycle Bin was cancelled"). The flag
  overrides `FOF_NOCONFIRMATION` for that one question; the recycle is
  otherwise as silent as it was. The question is out of Open questions.
- (finish) 2026-09-30 — W4.8's df-app half. "Empty trash" where the trash
  is the system's own bin (`platform::trash::SYSTEM_BIN` is `Some`: Windows)
  opens `ConfirmKind::EmptyBin`, the Empty trash card counting instead of
  listing: "Empty the Recycle Bin? 3 items · 1.2 MB will be deleted for
  good.", the count and the bytes from `df_core::platform::trash::bin_size()`
  in the card's own words (`trashview::weight_text`), no names under the
  question, Cancel and Empty. Its yes runs `empty_bin()` as a job, which says
  "Emptied the Recycle Bin" or the shell's error; an empty bin opens no card
  and says "The Recycle Bin is already empty". There being no trash view on
  Windows, the palette's "Empty trash" row, which Linux offers only while the
  trash view is on screen, is always there on Windows, its detail "Recycle
  Bin" rather than a count, so opening the palette asks the shell nothing.
  The gate in `App::refusal` for a platform whose trash was a stub (S1.34),
  which has refused nothing since W4.7, is gone. `u` after a trash says
  "Restore it from the Recycle Bin" (de6769e), which `d` now reaches. Linux
  and macOS are unchanged: the new path is taken only where `SYSTEM_BIN` is
  `Some`, and the gate refused only a home trash that was `Unsupported`,
  which neither has.
- (finish) 2026-09-30 — W4.38: through `df_core::platform::process::quiet`
  go 7-Zip's two listings in `preview/listing.rs` (`7z x -so` into the tar
  reader, `7z l -ba -slt`), the fixture maker of that file's tests, which
  the task names (`:930` then), and fd and rg behind the search panel
  (`search::build`), which a grep for `Command::new` does not find, that
  module naming `Command` `Process`. Left as they are: the openers
  (`platform/{unix,windows}/open.rs`) and a new window (`window.rs`), as the
  task says; the typed line, under `CREATE_NO_WINDOW` since W4.3; what only
  Linux's and macOS's own modules start (`gio`, `wl-copy`, `wl-paste`, `sh`,
  the portal's window, `ps`), which never runs on Windows; and the fixtures
  of other tests (`app.rs`'s 7z, `preview/decode.rs`'s ffmpeg and bash),
  which are not the program. A tar's listing is df-core's (`archive::list`,
  bsdtar through `quiet` since W4.2). Linux: `quiet` does nothing there.
- (finish) 2026-09-30 — W4.32, the rclone daemon's transport on Windows:
  option (a), Windows' own `AF_UNIX`, decided by Brian (the question is out
  of Open questions). `platform/windows/socket.rs` is a WinSock stream socket
  (`WSASocketW` not inherited by children, `connect` with a `SOCKADDR_UN`
  holding the path's UTF-8 bytes, as rclone's Go hands it over, `select`
  with the timeout before each `recv` and `send`), owned by std's
  `OwnedSocket` so its drop is the one `closesocket`. The socket's
  directory is
  `%LOCALAPPDATA%\delightfile\run`, made with the ACL it inherits (the user's,
  SYSTEM's and the administrators'), a link or junction in its place
  refused; no DACL of our own. Where the socket may go became a contract
  function, `platform::socket::dirs(uid)`: on Unix the three places
  `vfs::rclone::socket_path` listed, moved there unchanged, so Linux looks
  where it looked; on Windows that one folder. `vfs/http.rs`'s client is
  unchanged; its socket tests serve on a test-only
  `platform::socket::Listener` (`UnixListener` on Unix) and run on Windows
  too. rclone's tests
  (`vfs/rclone_tests.rs`) lose their module-wide `cfg(unix)`; `find_rclone`
  looks for `rclone.exe` there; the three that reach for Unix itself keep
  `cfg(unix)`. The killed daemon has a Windows twin of the same name (its
  pid from rclone's `core/pid`, ended by `taskkill /F`). The daemon dying
  with its thread has none: the job object ties the daemon to the process
  on Windows (W4.31). The swept partials have none either: the test needs
  a source rclone can stat and not read, and on the runner rclone copied
  both a file held open with nothing shared and one whose ACL denied
  everyone its data (`icacls /deny *S-1-1-0:(RD)`), which the test process
  itself could not read. Found on the way: `SO_RCVTIMEO` did not end a read
  on an `AF_UNIX` socket there (a test waited past a minute), so the socket
  waits in `select` before each `recv` and `send`; `FlushFileBuffers`
  refuses a handle opened only to read, so a download is flushed through
  `platform::fs::sync_file`, which on Windows opens it to write (on Unix
  the read-only `fsync` the download had, moved). The Windows job installs
  rclone.portable 1.75.1 from Chocolatey, the real `rclone.exe` ahead of
  Chocolatey's shim on `PATH`, and shows rclone serving on a unix socket
  before the tests run.
- (finish) 2026-09-30 — The finish branch, `port/windows-finish`, rebased
  onto `main` at 4c0012c (the release tooling and W4.39's title bar): W4.7's
  nuke warning (44429aa), W4.8's df-app half (2225ac7), W4.38 (d43e913),
  W4.32 (09a2d88), the README (a9d6d4f), and da51f03, the preview worker
  test waiting for its bell as the watcher tests do, a race the Windows
  runner showed once on this branch. Run 36737465177 is green on all three
  jobs: on Windows df-core passes 1,101 tests and 1 ignored (1,081 at run
  36724365335, before W4.32 un-gated rclone's and the socket's), df-app
  1,164, and clippy and the smoke test pass; Linux passes df-core's 1,187
  and 1 ignored and df-app's 1,308, 2 and 1. W4.34 is left as its entry
  says. The VM check this round was to make did not happen: at the first
  look the VM was at its sign-in screen, whose password no agent enters,
  and at the second the viewer stopped answering (07 §5.8).
- (tidy) 2026-09-30 — W4.32, a daemon dropped while a transfer runs,
  decided for Brian, who delegated it; the question is out of Open
  questions. `TerminateProcess` gives rclone no chance to remove the
  `.partial` it was writing, so on Windows the drop first stops the job the
  daemon started and has not seen end, through the `stop()` a cancel uses,
  and then ends the daemon as before. rclone removes a stopped job's
  `.partial` itself; the Windows runner already showed that in the upload
  half of the cancel test, which sweeps nothing. Each request of that stop
  waits at most 250 ms (`LAST_STOP_ANSWER`), so a daemon that no longer
  answers costs one such wait: on Linux, with `terminate` made `SIGKILL` for
  the experiment and the daemon held with `SIGSTOP`, the drop took 261 ms.
  Not `core/quit`, the question's option (a): rclone 1.75.1 answers it and
  then sleeps a fixed 1.5 s (`rcQuit` in `fs/rc/internal.go`) before it runs
  its exit handlers, which remove the partials, and exits. That measured
  1,516 ms mid-copy and 1,519 ms idle, so every drop on Windows would have
  cost 1.5 s, and a wait of about a second would have ended in the kill
  every time. Not `job/list` either: the daemon knows its one running job,
  since the worker runs one command at a time. Whether `terminate` is gentle
  is `platform::process::TERMINATE_IS_GENTLE`, true on Unix and false on
  Windows, so there is no `cfg` outside `platform/`, and Unix's drop is
  unchanged: `SIGTERM`, nothing before it. Linux still tracks the running
  job and never reads it. `a_daemon_dropped_mid_copy_cleans_up_after_itself`
  loses its `cfg(unix)` and runs on the Windows runner. Checked on Linux with
  `terminate` made `SIGKILL`: with the job stopped first the test passes in
  0.13 s, and without the stop rclone leaves `big.bin.<hex>.partial` behind
  and the test fails.
- (polish) 2026-10-01 — W4.40: the bookmarks are a function of the seam,
  `platform::defaults::bookmarks() -> Vec<config::Bookmark>`, where they were
  a constant, since a known folder's path is the shell's answer at start;
  Linux's and macOS's are their constants, mapped. A `Bookmark` gains
  `name: Option<String>`, the Places card's name for a row of the shipped
  table where the folder's own would mislead (Home for the profile, whose
  folder is the user's name; AppData for `AppData\Roaming`), `None` for
  every `[goto]` row a person writes. The which-key line is "<name> · <path>"
  for Windows' named rows and "Drive C:\" for `g c`. A known folder the
  shell cannot name and that is not under the profile either is left out.
- (polish) 2026-10-01 — W4.40: a place under home is shown whole on Windows
  everywhere a list shows one, not only on the which-key card and the Places
  card the brief named: `g space`, `z`, a tab's row in the finder and the
  sync card's title go through the same `finder::shorten_home`, and one
  place spelled two ways in two lists would be the inconsistency the round
  is about. This replaces W4.11's `~\Work` in what is shown;
  `platform::os::HOME_AS_TILDE` is the switch, `true` on Linux and macOS,
  whose lists are unchanged. A pin is still written from `~` in the state
  file (`finder::tilde`), so the file reads the same on every machine.
- (polish) 2026-10-01 — W4.40: `g /` is Windows' row in `KEYMAP_OVERRIDES`
  for the `mount-manager` command, "Places: drives and network". Linux and
  macOS ship no `g /` (the brief took Linux's to go to the root; it does
  not exist), and none is added there.
- (polish) 2026-10-01 — W4.41, the principle, as Brian put it: Enter does
  what a double-click in Explorer does, for every kind, archives and text
  included — the brief's "archives: Extract… first" is read as the head of
  `O`'s alternatives, after `open`, since Explorer's double-click on a zip
  opens it as a folder and Enter there is that. A font is the exception the
  brief makes (W4.43). Executables and scripts run through `run`, the same
  `builtin:shell-open` under the word "Run", so `.ps1`'s Enter is whatever
  Windows does with one (Notepad, by default) and `O` has `edit`.
- (polish) 2026-10-01 — W4.41: Paint is not offered for an SVG, which it
  cannot read; VS Code is, as the brief also said. `notepad` and `paint`
  take `$1` only, their programs opening one file. `terminal-at` stays a row
  no rule names, so a `[[open.rules]]` written for it still finds it; files'
  rules end in "Open with…" instead.
- (polish) 2026-10-01 — W4.41: D5.4's candidates are found on `PATH` or in
  `App Paths` (`HKCU` then `HKLM`, `Software\Microsoft\Windows\CurrentVersion\
  App Paths\<name>.exe`, `platform/windows/known.rs`), because Notepad++,
  VLC and 7-Zip register themselves there and not on `PATH`, and a row
  found only by `PATH` would be absent on nearly every machine that has
  them. A row found there runs its program by the registered path, quoted,
  since `CreateProcess` reads `PATH` and not the registry. A Store app's
  execution alias under `WindowsApps` (the new Paint, Terminal) counts as
  there.
- (polish) 2026-10-01 — W4.41: "Open with…" was `rundll32
  shell32.dll,OpenAs_RunDLL "$1"`, as the brief said, until the VM showed
  it (§5.8): on 0c93a72's build, chosen on `notes with space.txt` in the
  share, no chooser came up. Nor did one for the shell's `openas` verb
  through `ShellExecuteW` on 6478f43's (`builtin:shell-open-with`,
  63b0a90), while Explorer's own "Choose another app" on the same file put
  up "Select an app to open this .txt file". It is `SHOpenWithDialog` now
  (b2989b8), the function that chooser is, and on b2989b8's build it came
  up and closed unanswered with no error. Why the first two showed nothing
  was not found; `OpenAs_RunDLL` reading its quotes as part of the path is
  a guess, and `openas` gave no error to toast. `SHOpenWithDialog` is modal and
  waits to be answered, so it is put up from a thread of its own, with COM
  and no owner window, and the window goes on; a failure in its first
  250 ms is the window's toast, a close unanswered no error. It is asked
  for Always as well as Just once (`OAIF_ALLOW_REGISTRATION |
  OAIF_REGISTER_EXT | OAIF_EXEC`), as Explorer's has them; on the VM
  Windows showed Just once alone, which is Windows' to decide.
- (polish) 2026-10-01 — W4.42: the drawings are their own module,
  `marks.rs`, rather than more of `glyphs.rs`: a glyph's stand-in is placed
  in a line of text by its advance and baseline, a mark in an icon column by
  a box. The box is the Nerd Font picture's, measured off JetBrainsMono Nerd
  Font as egui draws it at 12.5 and 44 points: about an em wide from the
  anchor, 0.84 em tall, centred on the anchor's middle as the picture's ink
  is, which stands its foot on the name's baseline in a row. A grid tile's
  mark is centred on the tile; the Nerd Font glyph there sits 0.2 em right of
  centre (its picture is wider than its advance), and still does.
- (polish) 2026-10-01 — W4.42: `icon_for` and the icon constructors lose
  their `nerd` argument; an `Icon` always carries the face's glyph and its
  `Mark`, and `icons::paint` chooses at the paint. The `ls -F` classifiers
  are removed rather than kept "where text is the output": nothing outputs
  an icon as text. The padlock on a locked archive member is drawn too,
  where it was the key emoji, and the Places card always has its icon
  column, so its empty line is indented to the names, as it was with a Nerd
  Font.
- (polish) 2026-10-01 — W4.43: `builtin:font-install` is `ShellExecuteExW`
  with `SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_INVOKEIDLIST`,
  on the engine's micro lane, one job a font, the job's thread keeping its
  apartment and pumping its messages until the font is in a fonts folder
  (`platform::fonts::dirs`) under its file name, a process the verb started
  has ended, or 30 s have passed. In is "Installed <family style>", the name
  off the face (`preview::doc::font::facts`), its file name where it has
  none; not in fails the task, "Windows was asked to install <name>, and
  <n> s later it is not in a fonts folder", so the tasks panel keeps why
  after the toast has gone. A missing verb (`ERROR_NO_ASSOCIATION`, `SE_ERR_NOASSOC`)
  opens the Font Viewer: "Opened <name> in the Font Viewer: this Windows
  has no Install for it". The portable half, `open::install_font`, is
  df-app's; the platform gives `install_verb` and `INSTALLS_FONTS`.
- (polish) 2026-10-01 — W4.43: the runner (GitHub's Windows Server image)
  has no association for `.ttf` at all — `AssocQueryStringW` finds no
  `open` and no `install`, and `install_font` there ends in "no program is
  set to open it" — so the runner cannot show the verb; its tests print
  what they find and skip the install, and the Windows 11 VM is where it is
  seen (§5.8, 07 §5.7).
- (polish) 2026-10-01 — W4.43, from the VM (§5.8): the first two forms
  installed nothing, their tasks done and the font in no fonts folder,
  while Explorer's own Install was on the file's menu. e4a11d6's call
  returned and gave its apartment up at once, and the verb is a handler the
  shell runs in the calling process, its work done through the thread's
  messages; 0c93a72 kept the apartment and pumped, and still nothing.
  6478f43 installed it: the verb looked for in the item's shortcut menu,
  handlers included (`SEE_MASK_INVOKEIDLIST`), as Explorer's menu and
  `FolderItem.InvokeVerb` find it, and `SEE_MASK_FLAG_NO_UI` no longer set,
  under which a handler may do nothing rather than ask. Which of the two
  did it was not separated — there is no Windows here to try one without
  the other, and each VM build is a runner's round — so both are kept, and
  the pumping with them.
- (polish) 2026-10-01 — W4.44: the dependency rule's exception, logged as
  `00-ground-rules.md` §3 asks: resvg 0.48 (`text`, `memmap-fonts`; no
  `svgz`, raster images or fontconfig), an SVG renderer — an XML and CSS
  parser, a path, gradient and filter rasteriser and a text shaper — being
  what "rewriting is impractical" was written for. Under
  `[target.'cfg(not(target_os = "linux"))'.dependencies]`, so Linux builds
  none of it (`cargo tree -p df-app -i resvg` finds nothing there); 28 crates
  join the Windows and the macOS tree each, 24 of them new to `Cargo.lock`.
  The shared body is `platform/svg.rs`, compiled for macOS and Windows,
  whose FFmpeg both lack librsvg; Linux's `platform::svg::render` answers
  nothing and FFmpeg draws as before.
- (polish) 2026-10-01 — W4.44: an SVG is drawn to fit the pane, enlarged as
  well as shrunk (a raster is never enlarged), at most 8192 pixels a side; a
  24-point icon fills the pane, as a vector can. Its words are set in the
  faces of `platform::fonts::dirs`, loaded once a process and only for a
  drawing with a `<text`, so an icon costs no font walk; fontdb's own generic
  families stand (Arial, Times New Roman, Courier New), which Windows and
  macOS both have.
- (polish) 2026-10-01 — W4.45: the thumbnail is the `Thmb` record the
  header's pointer (the `u64` at offset 24) leads to, not the largest PNG in
  the file, as the brief had it. Read off Brian's seven files in `~/Windows`:
  a document keeps the raster pictures placed in it as PNGs too, and two of
  the seven had one larger than the thumbnail — a book cover's painting at
  1023×1537 without the title beside the 320×512 thumbnail of the cover, one
  logo of seven at 2733×570 beside the 512×300 sheet — so "largest" would
  show a part of the document. The largest well-formed PNG is kept as the
  last resort, where no record is found in the first 16 MiB. Following the
  pointer reads only the thumbnail, so a large `.afphoto` costs a seek and a
  few hundred kilobytes.
- (polish) 2026-10-01 — W4.45: the card for a file with no thumbnail is a
  new `Preview::Card` (what, name, size, modified, created) that df-core
  makes from the entry, painted as the audio card's lines are; the caption
  over a thumbnail is the kind badge's chip, "embedded preview", the words
  the app has for a corner caption, there being no other derived-preview
  caption to borrow. An Affinity file's kind is a picture's in the list
  (`FileKind::Image`), and its openers are the fallback rule's on every
  platform.
- (polish) 2026-10-01 — W4.46: `df_core::path::native` is applied where a
  place is written by a person or a table — `expand_home` (bookmarks, pins,
  an SSH key file, whose `-i` is now `\`-separated on Windows), `Go to:`'s
  `typed_path`, a `[goto]` row's which-key words — and nowhere a path is
  read from the disk, which is already Windows'. A pin is still written with
  `/` after `~` in the state file, as W4.11 has it.
- (polish) 2026-10-01 — Not of the round: today's stable Rust deprecates
  `AtomicUsize::fetch_update` for `try_update`, and the macOS and Windows
  runners, on current stable, refused the one call in `app.rs` under
  `-D warnings` — red on `main` since 849a416. It is a compare-and-swap loop
  now (557a184), since Arch's 1.98 has no `try_update`.
- (repaint) 2026-10-01 — W4.47: the brief suspected the loop (a deadline
  timer, `WaitUntil`, `RedrawRequested` on Windows), so the loop was
  measured first. The probe logged winit's `StartCause`, the rest
  `about_to_wait` chose and every repaint decision. A key from idle drew a
  frame slower than a refresh (about 20 ms on WARP, the log going to the
  share), asked for the next on the way to sleep with the loop still on
  `Wait`, and the next came 4 ms later (`WaitCancelled`): the `WM_PAINT`
  that `request_redraw` leaves does end `MsgWaitForMultipleObjectsEx`. The
  toast's fade deadline 1.65 s on woke the loop as `ResumeTimeReached`. A
  rest that never sleeps on a frame owed now was written and dropped, there
  being nothing for it to fix.
- (repaint) 2026-10-01 — W4.47: a detour, kept here so it is not taken
  again. Screenshots taken seconds after a key showed no toast while the log
  had drawn one, and Fifo was tried (the probe with `DF_PRESENT=fifo`, then
  a build of 6efe128, run 36930234032, that took it on Windows: a sync
  interval of 1, where Mailbox's is 0). Fifo's screenshot right after `u`
  had the toast both times where Mailbox's had not, and a screenshot a
  second after `p` had none under either. Read against the log, each of
  those screenshots fell after the toast's 2.2 s or inside them by chance;
  the viewer, not the present mode, was the difference, and the states that
  last (a bar, a row, an 8 s toast) showed Mailbox's frames on screen. Fifo
  was dropped, and Windows presents with Mailbox as before.
- (repaint) 2026-10-01 — The black around an SVG on Windows (W4.44) is not
  resvg's: `platform::svg::render` hands back alpha 0 where the drawing is
  clear (`a_transparent_drawing_is_clear_around_its_picture`, on both
  runners). The first visit to a file writes its picture to the yazi cache
  as a JPEG (`decode::write_thumb`, `to_rgb8`), which has no alpha, so what
  was clear is black there; every later visit shows that JPEG as the
  placeholder and draws the real picture over it at full strength
  (`paint::media_body` keeps the placeholder under the picture), and the
  black shows through the clear parts. Seen on the VM: a disc clear on its
  first visit and over a black square in the next run (§5.8). Nothing in it
  is Windows': the same code writes and reads the cache on Linux, for a
  transparent PNG as for an SVG (read in the code, not looked at on a Linux
  screen), so the fix is a decision (Open questions).

## Open questions

- Whether the Windows runner's clipboard tests are stable enough to be required
  (W4.16); decide after the first ten runs.
- Whether `wt.exe` is a safe default terminal (`05-defaults-and-config.md` §2): it
  is absent on a fresh Windows 10 LTSC.
- (polish, 2026-10-01) The row menu's Open with ▸ lists a file's openers by
  their ids (`install-font`, `font-viewer`, `open-with`, `notepad`), where
  `O`'s picker has their descriptions ("Install", "Preview in Windows Font
  Viewer", "Open with…"); Linux's does the same (`zed`, `edit`), so it is
  not of this round. Options: the descriptions in the menu on every
  platform (a Linux change); friendlier Windows ids; leave it.
- (repaint, 2026-10-01) A picture with clear parts shows black through them
  from its second visit on, on every platform: the yazi-cache JPEG its first
  visit wrote is drawn under it (Decisions log, W4.44). Options: (a) the
  placeholder is dropped once the picture has faded in over it — one line
  in `paint::media_body`, but the black still shows through the crossfade
  and goes at its end; (b) the decode worker says whether a picture has a
  pixel that is not opaque, the pane drops the placeholder the moment such
  a picture lands, and no JPEG is written for it — no black once the
  picture is up, an alpha scan per decode on the worker, and such a file has
  no cached thumbnail of delightfile's making, so the grid decodes it whole;
  (c) on macOS and Windows only, an SVG drawn by resvg is neither written
  to nor read from the cache — Linux untouched, but a transparent PNG keeps
  the black everywhere, and so does an SVG on Linux. (a) and (b) change what
  Linux shows for such a file.
