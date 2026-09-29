# 01 — The platform seam

Status: **in progress**

Scope: make every crate in the workspace **compile and pass its tests on
`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`**,
by moving every Linux-specific body behind `df_core::platform` and
`df_app::platform`, with honest stubs (`00-ground-rules.md` §2) where a native
implementation is a later phase. No feature gains a native macOS or Windows body in
this phase except where the body is trivially the same libc call (localtime, uid) or
trivially nothing (fadvise). The purpose is to cut along the seam once, correctly,
with Linux behaviour unchanged, so that Phases 2 and 4 fill in bodies without
touching callers.

Factual basis: `appendix-inventory-df-core.md` §1–§2 and
`appendix-inventory-df-app.md` §1–§2. Every task names the appendix rows it closes;
when done, suffix those rows with `✓ S1.n`.

Exit criterion for the phase: the CI matrix (`06-build-and-release.md` B6.1–B6.5) is
green on all three targets with `continue-on-error` removed, and the Linux test count
has not dropped.

## Decisions already made

- **Module layout** is fixed by `00-ground-rules.md` §2. `platform/mod.rs` in each
  crate begins with a table of every function it exports; that table is the contract
  and is updated in the same change as any signature.
- **The Linux watcher, trash, D-Bus, Wayland and portal code move as whole files.**
  `fs/inotify.rs` → `platform/linux/inotify.rs`, `fs/watch.rs`'s run loop →
  `platform/linux/watch.rs`, `ops/trash.rs` → `platform/linux/trash.rs`,
  `df-app/src/wayland/` → `df-app/src/platform/linux/wayland/`, `dbus.rs`, `portal/`
  and the udisks half of `mounts.rs` likewise. `git mv` so history follows. The
  public types callers use are re-exported from their old paths (`df_core::fs::watch::Watcher`,
  `df_core::ops::trash::TrashedItem`) so df-app's imports do not churn in this phase.
- **Stubs on macOS and Windows for**: file watching (`Watcher::new` →
  `Err(Unsupported)`, `Watcher::start` already falls back to `disabled()`), trash
  (every operation `Unsupported`; `available_for` false), reflink (`Ok(false)` →
  chunked copy), `is_remote` (false), owner names (`None`), thread priority (no-op),
  SFTP transport on **Windows only** (`Unsupported`; macOS keeps the real `poll` body),
  rsync sync on **Windows only** (`available()` false), clipboard/drag-out (no-op with
  the existing `wl-copy`-absent fallback path returning an error the UI already
  toasts), mounts (empty list), portal and `--portal` (absent), udisks (absent).
- **Native in this phase, because the body is one libc call that exists on every
  target**: `localtime` (`localtime_r` on Unix, `localtime_s` on Windows), `uid`
  (`getuid` on Unix; on Windows a user-name string, see S1.8), errno classification
  (S1.3, because the macOS numbers differ and a wrong "transient" class retries a
  permanent failure).
- **`DfError::Unsupported(&'static str)`** is added once, in S1.1, and is the only
  `DfError` change.
- **No behaviour change on Linux.** Moved code is diffed against its origin
  (`git diff -M` shows renames); the reviewer checks that the diff inside a moved
  file is `use` lines and the `#![allow(unsafe_code)]` header.

## 1. Skeleton (do first, in one change)

- [x] **S1.1** `crates/df-core/src/lib.rs:46–82`: add
      `#[error("{0} is not available on this platform")] Unsupported(&'static str)`
      to `DfError`. Done when: it exists and `cargo test -p df-core` is green.
      — done 94131a7
- [x] **S1.2** Create `crates/df-core/src/platform/{mod.rs, unix/mod.rs, linux/mod.rs,
      macos/mod.rs, windows/mod.rs}` and `crates/df-app/src/platform/{mod.rs,
      linux/mod.rs, macos/mod.rs, windows/mod.rs}` with the selection boilerplate:
      ```rust
      #[cfg(target_os = "linux")] mod linux;
      #[cfg(target_os = "linux")] pub use linux::*;
      #[cfg(target_os = "macos")] mod macos;   // …
      #[cfg(windows)] mod windows;            // …
      #[cfg(unix)] mod unix;                  // shared Unix bodies, used by linux/ and macos/
      ```
      and the contract table in each `mod.rs` listing every submodule and function
      that the following tasks add (fill the table as you go; it is the phase's
      checklist inside the code). Done when: both crates compile on Linux with the
      empty modules and the CI workflow from B6.1 exists.
      — done: the df-core half 94131a7, the df-app half 6fa7daf, the B6.1
      workflow 9afe895 (closed at integration; the workflow's first run is
      S1.40's). Linux verified, other targets unverified until CI
- [x] **S1.3** `platform::errno` (df-core): `is_cross_device`, `is_exists`,
      `is_not_empty`, `is_not_dir`, `is_dir`, `is_invalid`, `is_transient`, each
      `fn(&io::Error) -> bool`. Unix body compares `raw_os_error()` with the
      `libc::E*` constants (correct per target by construction; the current
      hard-coded `{11, 16, 23, 24, 26, 116}` in `tasks.rs:349–363` becomes
      `EAGAIN, EBUSY, ENFILE, EMFILE, ETXTBSY, ESTALE` by name). Windows body uses
      Win32 codes: `ERROR_NOT_SAME_DEVICE` 17, `ERROR_FILE_EXISTS` 80 /
      `ERROR_ALREADY_EXISTS` 183, `ERROR_DIR_NOT_EMPTY` 145, `ERROR_DIRECTORY` 267,
      `ERROR_INVALID_PARAMETER` 87, transient = `ERROR_SHARING_VIOLATION` 32,
      `ERROR_LOCK_VIOLATION` 33, `ERROR_TOO_MANY_OPEN_FILES` 4, `ERROR_BUSY` 170.
      Replace every `raw_os_error() == Some(libc::E…)` outside `platform/`:
      `ops/copy.rs:473, 740, 754`, `ops/trash.rs:178–184, 462–468` (moves with the
      file, but the *move-cross-device* helper it calls is shared and uses the new
      predicate), `tasks.rs:349–363`, and the `EEXIST`/`ENOTEMPTY`/`ENOTDIR`/`EISDIR`
      uses the appendix lists under `ops/*.rs`. Done when: `grep -rn "libc::E" crates/df-core/src`
      hits only `platform/`. — done 688d691; the grep holds from 57ce63d, once
      S1.18 (`fs/tags.rs`), S1.19 (`ops/mode.rs`'s descriptor walk) and S1.50
      (`vfs/child.rs`) had moved the last hits.

## 2. df-core: filesystem primitives

- [x] **S1.4** `platform::watch`: move `fs/inotify.rs` (whole file) and the run loop
      of `fs/watch.rs:94–111, 147–167, 181–301` into `platform/linux/watch.rs` +
      `platform/linux/inotify.rs`. `fs/watch.rs` keeps `WatchEvent`, `Notifier`, the
      `Watcher` struct's public methods (`new`, `start`, `disabled`, `is_active`,
      `watch`, `events`, `drain`, appendix §2 `fs::watch`) and delegates to
      `platform::watch::Backend` (a type each platform defines with
      `open(control, events, notify) -> io::Result<(Backend, JoinHandle<()>)>`, which
      starts the thread body, and `wake()`; `set_watches` stays a private helper of
      the Linux thread). macOS and Windows
      `Backend::open` return `Err(io::Error::from(ErrorKind::Unsupported))`; the log
      line at `:122` that names inotify becomes platform-neutral text with the error
      appended. Done when: the two `Watcher` tests in `fs/tests.rs` that start a real
      watcher pass on Linux
      and are `#[cfg(target_os = "linux")]`-gated until Phase 2/4 add bodies; df-core
      compiles on all three targets. — done ba40fba; the whole crate compiles for
      both foreign targets from 5811c6b and its tests from ae24f85, cross-checked locally,
      CI pending
- [x] **S1.5** `platform::fs` (df-core), moving these bodies out of `ops/copy.rs`,
      `ops/link.rs`, `ops.rs`, `sync/execute.rs`, `vfs/conn.rs`:
      - `reflink(reader: &File, writer: &File) -> bool` — Linux:
        `ops/copy.rs:54, 581–594` (FICLONE) unchanged; macOS/Windows stub `false`.
      - `symlink(target: &Path, link: &Path) -> io::Result<()>` — Unix:
        `std::os::unix::fs::symlink`; Windows: `symlink_dir` when
        `target`-resolved-from-`link.parent()` is a directory, else `symlink_file`.
        Callers: `ops/copy.rs:231–244`, `ops/link.rs`, `ops/journal.rs`
        (`redo_links`, which remakes an undone link), `test_support.rs:61–68`.
      - `apply_mode(path: &Path, mode: u32) -> io::Result<()>` — Unix:
        `ops/copy.rs:629–635`; Windows: set the read-only attribute when
        `mode & 0o222 == 0`, clear it otherwise.
      - `set_times(path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()>` —
        Unix: `ops/copy.rs:651–688` with `UTIME_OMIT` taken from **`libc::UTIME_OMIT`**
        rather than the hard-coded `0x3ffffffe` (appendix flags the macOS value is
        `-2`; on Linux `libc::UTIME_OMIT` equals the current literal, so Linux is
        unchanged); Windows: `File::set_times` on a handle opened with
        `FILE_FLAG_OPEN_REPARSE_POINT`.
      - `sync_dir(dir: &Path) -> io::Result<()>` — Unix: `ops/copy.rs:464–479`;
        Windows: `Ok(())` (directories have no fsync).
      - `forget_cached(file: &File, path: &Path)` (the path is for its log line) — Linux: `sync/execute.rs:470–482`
        (`posix_fadvise`); macOS and Windows: no-op. (With `fs/inotify.rs` — S1.4 —
        this is one of the two df-core sites that do not compile on macOS today.)
      - `write_all_at(file: &File, data: &[u8], offset: u64)` — Unix: `FileExt`;
        Windows: `seek_write` loop. Caller `vfs/conn.rs:897`.
      - `same_file(a, b) -> io::Result<bool>` — defined in `03-paths.md` P3.3; create the Unix body here
        (`ops.rs:212–218` moved; `ops::same_file` is `.unwrap_or(false)` of it) and
        the Windows body in P3.3 (Phase 1 Windows: `Err(Unsupported)`).
      - `writable(dir: &Path) -> bool` — Unix: `sync/mod.rs:479–488` (`access(W_OK)`);
        Windows stub: `true` (the CRT's `_access(path, 2)` ignores read-only on
        directories, so it would say the same); W4.6 supersedes it with a real probe.
      - `is_remote(path) -> bool` and `magic_of` — Linux: the two functions from
        `du/fstype.rs` moved (the file also holds `REMOTE_FS_MAGIC`, `gvfs_root` and
        `on_device`, which stay, and re-exports the two); macOS/Windows stub
        `false`/`None` (Phases 2/4 fill).
      Done when: `grep -rn "std::os::unix" crates/df-core/src` hits only `platform/`
      (except the `MetadataExt` sites, which are P3.4) and Linux tests pass.
      — done e1e5e8c; the grep holds from bf80031 for everything but tests
      marked `#[cfg(unix)]` (S1.15), which name `std::os::unix` because they test
      Unix semantics, as `00-ground-rules.md` §6 has them do.
- [x] **S1.6** `platform::trash`: `git mv crates/df-core/src/ops/trash.rs
      crates/df-core/src/platform/linux/trash.rs`; `ops/trash.rs` becomes a shim
      that `pub use`s the portable surface. The portable surface is:
      `TrashedItem` with its **current** fields (`name`, `original`, `deleted_at`,
      `trash_root` — `crates/df-core/src/ops/trash.rs:62–72`; df-app constructs it at
      `tab.rs:1402` and `app.rs:19209`, so the field set does not change) plus a
      new method `location(&self) -> PathBuf` = where the item lives now (Linux:
      `files_path()`; macOS: the journal's recorded path, with `trash_root` = the
      journal directory; Windows: empty). The Linux type keeps `files_path()`,
      `info_path()`, `is_orphan()` as inherent methods and the other platforms
      provide the same methods returning what makes sense (`info_path` →
      `location()`, `is_orphan` → `false`). `Trash` with `home()`, `at()`, `trash()`, `list()`, `restore()`,
      `purge()`, and the free fns `for_path`, `restore`, `purge`, `suffixed`,
      `iso8601_utc`, and — since the plan was written — the trash-aging surface
      df-app calls: `Purged`, `purge_expired`, `purge_expired_if_due`,
      `purge_due_in`, `parse_deletion_date` (plus `Trash::{root, files_dir}`).
      `suffixed`/`fit`/`clip` are **not** trash-specific (paste and vfs
      use them): move them to `df_core::fs::names` in this task, with
      `MAX_NAME_BYTES` and `MAX_TRASH_COLLISIONS` (every caller of `suffixed` bounds
      its loop with it), re-exported from
      `ops::trash` for now. macOS/Windows stubs: `home()` and `for_path()` return
      `Err(DfError::Unsupported("Trash"))`; `list()` `Ok(vec![])`. The
      `sync/mod.rs:412–458` `trash_available`/`trash_for` move to
      `platform::trash::{available_for, for_sync}` with the same stubs (not
      `for_path`, which already names the `d` rule; the mirror's rule differs in
      refusing rather than falling back to the home trash). Done when:
      Linux trash tests pass from their new location, df-app compiles unchanged on
      Linux, and `d` on macOS/Windows would reach the refusal toast. The check is a
      df-app unit test gated `#[cfg(not(target_os = "linux"))]` asserting
      `App::refusal(Command::Trash)` is `Some` for a local path; it first runs for
      real on the CI matrix at S1.40. On Linux `available_for` is true for every
      local path, so there is nothing to assert there.
      — done: the df-core half 70ad913; the df-app refusal test,
      `app::tests::the_trash_is_refused_where_the_platform_has_none`, with
      S1.34, done 2544733. Linux verified, other targets
      unverified until CI (the test is compiled out on Linux and first runs on
      the macOS and Windows runners)
- [x] **S1.7** `platform::meta` — the `MetadataExt` surface is specified in
      `03-paths.md` P3.4. In this phase create the module with the **Unix body only**
      and a Windows body good enough to compile: `dev = 0`, `ino = 0`, `nlink = 1`,
      synthesized `mode`, zero `uid`/`gid`, `change_time` from `last_write_time`.
      (`std::os::windows::fs::MetadataExt::{number_of_links, file_index,
      volume_serial_number, change_time}` are **unstable** behind `windows_by_handle`;
      only `file_attributes`, the three times and `file_size` are stable. W4.5 gets
      the real values from `GetFileInformationByHandle` via `windows-sys`.)
      and route `fs/entry.rs:119–175`, `du/walk.rs:60–696`, `preview/cache.rs:137–151`,
      `archive/write/mod.rs:542–556`, `ops/link.rs` (nlink test helper),
      `ops/trash.rs` (`device_of`, moved) through it — and, since the plan was
      written, `ops/mode.rs` (the permissions change reads `st_mode`, `st_dev`,
      `st_ino`) and `ops/copy.rs`'s `apply_mode` (`PermissionsExt::mode()`, the same
      `st_mode`). `archive/write` also needs `st_mtime`, so the module has
      `mtime(&Metadata) -> i64` beside P3.4's list. Done when: no `MetadataExt`
      outside `platform/` in df-core, Linux tests pass. — done 57c5c7b; the grep
      holds from bf80031 (tests included; the last test sites read through
      `platform::meta`).
- [x] **S1.8** `platform::user`: `uid() -> u32` (Unix `getuid`; Windows: `0`, and
      **`cache_suffix() -> String`** used by `preview/cache.rs:128–132, 195–212` for
      `yazi-<suffix>`: Unix the uid, Windows ~~the `USERNAME` env var~~ `"0"` — yazi's
      own Windows convention, confirmed in its source, see the Decisions log). Callers: `ops/trash.rs:588–594`
      (moved), `sync/mod.rs:431, 443–458` (moved), `preview/cache.rs`, and
      df-app `mounts.rs:gvfs_root` (Linux-only after S1.20), and since the plan was
      written df-core's own `du::gvfs_root` and `vfs/rclone.rs`'s socket directory.
      Done when: no `libc::getuid` outside `platform/`. — done ed0530b,
      cross-checked locally, CI pending
- [x] **S1.9** `platform::thread::lower_priority(nice: i32) -> bool`: Linux body from
      `thread.rs:27–43`; the macOS body (a QoS class via
      `pthread_set_qos_class_self_np`, M2.5) is **not** done in this phase because it
      changes scheduling class rather than niceness and needs a live check; macOS and
      Windows stub no-op.
      Callers unchanged (they call `df_core::thread::lower_priority`, which now
      delegates; the clamp to 1–19 stays there, on every target). Done when:
      compiles on all targets. — done 8118fae, cross-checked locally, CI pending
- [x] **S1.10** `platform::time::local_civil(secs: i64) -> Option<Civil>` with the body
      of `rename/facts.rs:53–88`: Unix `localtime_r`; Windows `localtime_s` (present
      in the windows libc). Also df-app's `format.rs` localtime (appendix B) calls
      the same function. Done when: `Civil::local` tests pass on Linux; compiles on
      Windows. — done 39d8aa9, cross-checked locally, CI pending
- [x] **S1.11** `platform::dirs`: `home()`, `config_dir()`, `state_dir()`,
      `data_dir()`, `cache_dir()`, `runtime_dir()`, `temp_dir()`, returning
      `Option<PathBuf>`. Linux bodies moved from `config.rs:1400–1407`,
      `state/mod.rs:637–657`, `ops/trash.rs:110–116, 484–491`, `zoxide/mod.rs:117–126`,
      `vfs/config.rs:292–308`. macOS and Windows values are **defined in
      `05-defaults-and-config.md` §1** and implemented there (D5.1); in this phase
      the macOS body equals the Linux body (XDG with `$HOME` fallbacks, which is
      what yazi does on macOS) and the Windows body returns `None` for everything
      but `home()` (`USERPROFILE`) and `temp_dir()`. Since the plan was written,
      `du::gvfs_root` and `vfs/rclone.rs`'s socket directory read
      `$XDG_RUNTIME_DIR` too; they use `runtime_dir()`. Done when: callers use
      `platform::dirs`, Linux tests pass. — done 5d3fd8d, cross-checked locally,
      CI pending
- [x] **S1.12** `platform::pipe`: `git mv vfs/poll.rs platform/unix/pipe.rs`
      (`set_nonblocking`, `poll_read2`, `poll_write`). Windows: the module exists with
      the same three signatures returning `Err(Unsupported)`, and
      `vfs/conn.rs:184–188` (`Transport::spawn`) propagates that error so the vfs
      worker reports "SFTP is not available on this platform" through the existing
      `VfsError` path. Phase 4 replaces the Windows body with reader threads (W4.20).
      The module also has `AVAILABLE: bool` and `fd(&pipe) -> i32`: `vfs/conn.rs`
      took raw fds with `AsRawFd`, which Windows does not have, and on Windows
      `Transport::spawn` checks `AVAILABLE` before starting `ssh` and returns
      `VfsError::Spawn` with the refusal as its source ("could not start ssh:
      SFTP is not available on this platform") — no child started for nothing,
      and Linux's handling of a failed `set_nonblocking` (logged, not fatal) is
      unchanged.
      Done when: compiles on all targets, `vfs/tests.rs` passes on Linux. — done
      a22e68b, cross-checked locally, CI pending
- [x] **S1.13** `platform::process` (df-core): `NULL_DEVICE: &str`, `pause(&Child)`,
      `resume(&Child)` (each `-> io::Result<()>`; `sync::rsync` logs a refusal as it
      logged a failed `kill`), `is_executable(&Path) -> bool`, `candidates(name: &str) ->
      Vec<String>` (Unix: `[name]`; Windows: `[name.exe, name]` plus the
      `bsdtar` → `tar` alias, because Windows ships libarchive's bsdtar as
      `tar.exe` and GNU tar on Linux would not accept the same flags). Bodies:
      `sync/rsync.rs:631–675, 966–976` (SIGSTOP/SIGCONT; Windows `pause`/`resume`
      return `Err(Unsupported)`), `archive/external.rs:82–85` (`is_executable`,
      Windows: extension in `PATHEXT`), `archive/external.rs:75–78` and
      `archive/write/mod.rs:656–661` use `candidates`. `git/status.rs:291`
      `/dev/null` → `NULL_DEVICE` (`"NUL"` on Windows, W4.2's value, since the
      constant needs one now). Done when: no `libc::kill`/`SIGSTOP` outside
      `platform/`; archive tests pass on Linux. — done dde51fe; the grep holds
      from 57ce63d, which moved `vfs/child.rs`.
- [x] **S1.14** `sync::rsync::available()` (`sync/rsync.rs:69–77`) returns `false` on
      Windows without spawning (rsync is not a Windows tool; the `host:` endpoint
      syntax collides with drive letters). macOS keeps the real check; the version
      gate is Phase 2. Done when: a Windows-shaped unit test of the gating function
      exists (pure) and the function has no cfg outside `platform/` (put the
      constant `platform::process::HAS_RSYNC: bool` in the platform module).
      — done f81a714, cross-checked locally, CI pending (the pure gate is
      `sync::rsync::gated(has_rsync, probe)`; its test runs on every target)
- [x] **S1.16** Bytes ↔ UTF-8 at every `OsStrExt`/`OsStringExt` site outside
      `platform/` — **the mechanical half of `03-paths.md` §2, pulled into this
      phase** because without it df-core cannot compile on Windows and S1.40's exit
      criterion is unreachable. Create `platform::os::{as_bytes, from_bytes}` exactly
      as `03-paths.md` P3.1 specifies, then rewrite each site to call them,
      propagating the `Result` to the nearest `DfError` return and changing nothing
      else: `git/status.rs:41, 564`; `ops.rs:111, 198–203`; `ops/create.rs:36–43`;
      `state/mod.rs:746, 751`; `sync/mod.rs:315`; `sync/rsync.rs:43, 231, 398, 1146,
      1209`; `archive/write/mod.rs:536, 562, 580`; `ops/trash.rs:266–270, 613–632,
      682, 719` (moved by S1.6 but still Unix-only inside `fs::names`). The
      *semantic* work (separator handling, root, case, name validity) stays in Phase
      3, whose tasks P3.5–P3.10 then only have their behaviour to add. On Unix the
      bytes are identical (`as_bytes` is `OsStrExt::as_bytes` there). Done when:
      `grep -rn "OsStrExt\|OsStringExt" crates/df-core/src` hits only `platform/`
      and `cargo check -p df-core --target x86_64-pc-windows-msvc` passes on CI.
      Since the plan was written `fs/tags.rs`'s walk reads a name's first byte
      too; that site is rewritten here and the file's syscalls move in S1.18.
      — done e34eab8; the grep holds from bf80031 (tests included), and
      `cargo check -p df-core --target x86_64-pc-windows-msvc` passes locally, CI
      pending.
- [x] **S1.17** `platform::user::{user_name(uid), group_name(gid)} ->
      Option<&'static str>` (the shape `fs::owner`'s public functions already
      had; the sketch was `owner_names(uid, gid) -> (Option<String>,
      Option<String>)`) behind `fs/owner.rs:25–84`: Linux body is the existing
      `/etc/passwd`/`/etc/group` parser moved; macOS stub returns `None` (M2.4
      replaces it with `getpwuid_r`); Windows returns `None`. Also
      `ops/delete.rs:64` (the home rail reads `HOME` directly) → `platform::dirs::home()`
      (S1.11), and likewise `config::expand_home`, `vfs::config`'s key path and
      rclone config lookup, and a `preview::cache` test. Done when: `fs::owner`
      tests pass on Linux; no `"HOME"` literal in
      df-core outside `platform/`. — done 7a6e624
- [x] **S1.15** Tests that stop df-core's test target from *compiling* elsewhere:
      `test_support.rs:61–68` (`TempTree::symlink` → `platform::fs::symlink`);
      `vfs/tests.rs`, `sync/rsync.rs` tests and `archive/external.rs` tests that write
      `#!/bin/sh` scripts with `0o755` → `#[cfg(unix)]` on those tests; `du/tests.rs`,
      `ops/copy.rs`, `ops/link.rs`, `sync/tests.rs` tests that name `MetadataExt`/
      `UnixListener` → `#[cfg(unix)]`. Only compile errors are fixed here; tests that
      would *fail* on Windows for path reasons are Phase 3 P3.24. Done when:
      `cargo test -p df-core --no-run` succeeds on the macOS and Windows runners.
      Done as: `#[cfg(unix)]` on the tests that set modes, make links, read
      `st_ino`, serve a unix socket, run a `#!/bin/sh` stand-in, or build a
      non-UTF-8 path (`archive/external.rs`, `archive/write/tests.rs`,
      `du/tests.rs`, `fs/tests.rs`, `ops/copy.rs`, `ops/delete.rs`,
      `ops/journal.rs`, `state/tests.rs`, `sync/rsync.rs`, `sync/tests.rs`,
      `vfs/tests.rs`, `vfs/rclone.rs`; this covers the df-core half of S1.33's
      list), with the helpers only they use; `#[cfg(target_os = "linux")]` on
      `ops/jobs.rs`'s mode job test, since the walk is Linux's (S1.19). Shared
      fixtures go through the platform instead, so the tests that use them keep
      compiling everywhere: `TempTree::symlink` (`platform::fs::symlink`), the
      archive writer's `photos()` (`platform::fs::apply_mode`), `du/tests.rs`'s
      size helpers (`Metadata::len`, `platform::meta::blocks_bytes`), and
      `sync/tests.rs`'s inode check (`platform::meta::ino`). — done ae24f85,
      cross-checked locally (`cargo check -p df-core --tests` for both targets;
      linking needs the runners), CI pending
- [x] **S1.18** `platform::xattr` (added 2026-09-29: file tags postdate the
      inventory). `fs/tags.rs`'s four `l*xattr` calls (`lgetxattr`, `llistxattr`,
      `lsetxattr`, `lremovexattr`), their buffer sizing, the errno they read
      (`ENODATA`, `ENOTSUP`, `EPERM`, `ENOSYS`, `ERANGE`) and the two test hooks
      move to `platform/linux/xattr.rs` unchanged. `fs::tags` keeps the tag logic
      and `carry`, the `user.*` preservation copy, move and sync call; both reach
      the attributes only through the seam. macOS and Windows get
      `platform/stub/xattr.rs`: reads answer "no attributes" (`None`, an empty
      list), writes refuse, and `AVAILABLE` is false, so `tags::write` returns
      `TagError::Io(DfError::Unsupported("Tags"))` and `carry` copies nothing and
      reports nothing lost. No macOS body: whether macOS tags are
      `user.xdg.tags` or Finder's `com.apple.metadata:_kMDItemUserTags` is an open
      question in `02-macos.md`. Done when: the tags tests pass on Linux,
      `fs/tags.rs` names no `libc`, and df-core compiles on all three targets.
      — done 5ea059c, cross-checked locally, CI pending
- [x] **S1.19** `platform::nofollow`, the permissions change's descriptor walk
      (added 2026-09-29: `ops/mode.rs` postdates the inventory). Finding a path
      below the anchor without following a link — `O_PATH | O_NOFOLLOW`
      descriptors named through `/proc/self/fd/<n>`, `ready()` (was
      `proc_ready`), `Finder`, `open_in`, `stat_in`, `swapped`, `set_mode_of`, and
      a `read_dir_in` wrapper for the one `read_dir(named(..))` in `plan` — moves
      to `platform/linux/nofollow.rs` unchanged; `ops::mode` keeps the grid,
      the plan, the ordering, the record and the undo/redo checks, and `below`/
      `outside` (pure path logic the walk borrows). macOS and Windows:
      `platform/stub/nofollow.rs`, whose `ready()` refuses with
      "Permissions is not available on this platform" — the answer every
      change, undo and redo already gives when `/proc` is missing — and never
      falls back to a `chmod` by path. The tests that change modes on disk move
      to a Linux-gated `on_disk` module in `ops/mode.rs`; the grid tests run
      everywhere. Done when: `ops/mode.rs` names no `libc` or `O_PATH` in code,
      its tests pass on Linux, and df-core compiles on all three targets.
      — done a3ba8b0, cross-checked locally, CI pending
- [x] **S1.50** `platform::process::{tie_to_this_thread, terminate}` (added
      2026-09-29: the rclone daemon postdates the inventory; the brief called the
      first `tie_to_parent`, but the code's name is `tie_to_this_thread`, which
      says which parent it means and is what df-app's gvfs watcher calls through
      `vfs::child`, so it is kept). `vfs/child.rs` moves whole to
      `platform/linux/process.rs` (`prctl(PR_SET_PDEATHSIG, SIGTERM)` and the
      `getppid` check in `pre_exec`); `terminate` (`kill(SIGTERM)` on a child
      not yet reaped) moves on into the shared `platform/unix/process.rs`, so
      macOS has it. `vfs/child.rs` stays as a two-line re-export. macOS and
      Windows `tie_to_this_thread` is a no-op (`platform/stub/process.rs`) whose
      doc says the child can outlive a crashed parent there (M2.29, W4.31);
      Windows `terminate` is `Child::kill`. Done when: no `prctl`, `getppid` or
      `libc::kill` outside `platform/`; the rclone tests pass on Linux; df-core
      compiles on all three targets. — done 57ce63d, cross-checked locally, CI
      pending
- [x] **S1.51** `platform::socket`, the rclone transport (added 2026-09-29:
      cloud remotes postdate the inventory). `vfs/http.rs` connects to
      `rclone rcd` with `UnixStream`, and `vfs/rclone.rs` makes the socket's
      directory private with `DirBuilderExt::mode`, `MetadataExt::uid` and
      `PermissionsExt`. The connect-with-timeouts and `private_dir` move to the
      shared `platform/unix/socket.rs` unchanged, so macOS keeps the real
      transport (it is POSIX). Windows: `platform/windows/socket.rs`, whose
      `AVAILABLE` is false, so `Daemon::spawn` refuses before starting rclone with
      `VfsError::Spawn` whose source reads "Cloud remotes is not available on this
      platform" — the same path S1.12 gives SFTP — and whose `connect` and
      `private_dir` refuse the same way. The tests that serve or script a real
      socket are `#[cfg(unix)]` (`vfs/http.rs`'s `over_a_socket`, the whole
      `vfs/rclone_tests.rs`, and `socket_names_are_short_safe_and_distinct`).
      Windows transport: W4.32. Done when: `vfs/http.rs` and `vfs/rclone.rs`
      name no `std::os::unix` outside tests, the rclone tests pass on Linux, and
      df-core compiles on all three targets. — done 5811c6b, cross-checked
      locally, CI pending
- [x] **S1.52** df-core tests that compile everywhere but drive a body that is
      a stub off Linux (added 2026-09-29 at integration: S1.15 made df-core's
      tests *compile* for macOS and Windows, and these would then fail on both
      runners). Gated `#[cfg(target_os = "linux")]`, each until the task that
      gives the body, which removes the gate in the same change:
      - the trash through the journal and the job (M2.8, W4.7):
        `ops/journal.rs` `undo_of_a_trash_restores_it`,
        `undo_of_a_trash_refuses_when_the_name_came_back`,
        `redo_of_a_trash_trashes_it_again_into_the_same_trash`,
        `a_partly_undone_trash_leaves_only_the_remainder_and_clears_the_redo_stack`,
        `redo_of_a_trash_refuses_a_file_replaced_under_the_same_name`, and
        `ops/jobs.rs` `a_trash_job_is_undoable`;
      - the mirror's trash (M2.8, W4.7): `sync/tests.rs`
        `a_mirror_trashes_the_extras_after_copying_and_before_verifying` and
        `a_mirror_moves_a_folder_in_the_way_to_the_trash_and_copies_the_file`,
        the two that set `Removal::Trash`. The other tests built on
        `with_extras` run everywhere and are not gated:
        `removals_are_the_topmost_extras_deepest_first_with_what_each_takes`
        only plans, `a_mirror_deletes_for_good_where_there_is_no_trash` and
        `a_mirrors_progress_counts_each_removal` delete for good,
        `an_update_removes_nothing_whatever_the_plan_found` removes nothing
        (and `a_removal_that_fails_is_recorded_and_the_rest_carry_on` was
        already `unix`).
      Already gated, and checked: the chmod walk's disk tests (`ops/mode.rs`'s
      `on_disk` module and `ops/jobs.rs a_mode_job_goes_inside_and_can_be_undone`,
      S1.15 and S1.19; M2.28 un-gates) and the two watcher tests in
      `fs/tests.rs` (S1.4; M2.1, W4.4). Not gated, because they do not reach a
      stub: every test that writes a tag asks `tags::supported_here` first,
      which the xattr stub answers `false`, so it prints "skipping" and
      returns; the tag tests that write nothing are pure. Tests that reach a
      stub on **Windows only** are left with the Windows path failures to
      Phase 3 (P3.24), since that job is red for those anyway: `same_file` is
      `Unsupported` there until P3.3, which `ops.rs
      same_file_sees_through_a_symlinked_route`, `ops/copy.rs
      refuses_to_copy_a_file_over_itself`, `sync/tests.rs
      the_rails_refuse_what_no_sync_can_do_safely` (its "onto itself" case)
      and `ops/paste.rs same_directory_copy_auto_suffixes` rely on; the SFTP,
      rclone and rsync tests are already `unix` or skip (S1.15, S1.51, S1.14).
      The gated tests spell `crate::ops::Trash::at` in full, so their modules'
      imports are used on every target. Done when: none of df-core's tests
      reaches a Linux-only stub on the macOS or Windows runner, Linux's count is
      unchanged (the gated tests still run there), and `cargo check -p df-core
      --tests` is clean for both targets. — done afd0016, Linux
      verified (1138 with S1.26's new test; cross-checked clean for both
      targets), other targets unverified until CI
- [x] **S1.55** df-core linted for macOS and Windows as their CI jobs lint it
      (added 2026-09-29 at integration: the cross checks above are `cargo
      check`, and the jobs run clippy with `-D warnings`). The private
      toolchain (`00-ground-rules.md` §6) now has the clippy component, and
      `cargo clippy -p df-core --all-targets --target <t> -- -D warnings -A
      clippy::chunks_exact_to_as_chunks -A dead_code` — the jobs' own line —
      found one lint, six times, on both targets:
      `default_constructed_unit_structs` on `Finder::default()` in
      `ops/mode.rs`, because the `nofollow` stub's `Finder` was a unit
      struct. The stub's `Finder` now carries a private `()` field; Linux's
      `Finder` and `ops::mode` are unchanged. Done when: that command is clean
      for both targets. — done dc91116, cross-checked locally,
      CI pending

## 3. df-app: the seam

Factual basis: `appendix-inventory-df-app.md` §1 (per-file tables and the four
Linux-only module blocks), §2 (surfaces). The df-app seam is one handle plus a few
function tables. The single most important decision: **the Wayland `DataDevice`'s
shape becomes the portable `platform::desktop::Desktop`**, its `Event` enum moves
out to a portable definition, and the `wl-copy`/`wl-paste` *fallback* functions
become the portable synchronous clipboard API. This keeps `app.rs`'s clipboard and
drag state machine (appendix B §1 `app.rs` rows 12428–13455) untouched on Linux and
gives macOS and Windows a synchronous clipboard for free in later phases, because
their native clipboards *are* synchronous.

- [x] **S1.20** Cargo: move `wayland-client` from `[dependencies]` to
      `[target.'cfg(target_os = "linux")'.dependencies]` in `crates/df-app/Cargo.toml`
      (the workspace entry at `Cargo.toml:37` stays). Move `libc` to the same Linux
      target table **with S1.25, not before**: after S1.25 its remaining df-app uses
      are the nine in `wayland/mod.rs` (`pipe2`, `poll`, `read`, `write`,
      `memfd_create`), all Linux, but until then `format.rs`'s `localtime_r` needs it
      on macOS too (the first pass left it in `[dependencies]`; the second pass
      moves it in S1.25's change).
      **egui-winit**: change `Cargo.toml:62` to `egui-winit = { version = "0.35.0",
      default-features = false, features = ["wayland", "x11"] }`, dropping
      `clipboard` (arboard + smithay-clipboard: a second clipboard that reads the OS
      clipboard on egui's own Cmd/Ctrl+V and whose worker thread `App::exiting` has
      to outlive, appendix B intro) and `links` (webbrowser; df-app opens nothing
      through egui). This is a Linux-visible change and is logged: egui stops
      reading the clipboard on Ctrl+V, an event df-app never consumed, and the
      smithay-clipboard thread no longer exists to join. Change the two "for Wayland"
      description strings (`crates/df-app/Cargo.toml:3`, `main.rs:1–9`,
      `cli.rs:161`) to "for Linux, macOS and Windows" only when Phase 2 ships;
      for now leave them. Done when: `cargo tree -p df-app --target
      aarch64-apple-darwin` (on CI) shows no `wayland-*`. (`cargo tree` resolves
      without the target installed, so this one also runs on Linux.)
      — done 66814ec, Linux verified (`cargo tree` for both other targets names no
      `wayland-*`), other targets unverified until CI; `libc` moved to the Linux
      table with S1.25
- [x] **S1.21** `git mv crates/df-app/src/wayland crates/df-app/src/platform/linux/wayland`,
      `git mv src/dbus.rs src/platform/linux/dbus.rs`, `git mv src/portal
      src/platform/linux/portal`, and split `src/mounts.rs`: the model and card
      (`Device`, `Share`, `Address`, `Spec`, `Request`, `Reply`, `Card`, `Item`,
      `Line`, `Geometry`, `geometry`, `paint`, `Place`, `PLACES_EMPTY`, `ROWS`,
      `SCHEMES`, `connect_url`, `landing`, `attempt`, `gio_mounts`, `shares_from`
      — everything whose tests need no bus **and no `dbus::` type**; `devices_from`
      at `mounts.rs:175` takes `dbus::Interfaces` and moves with the worker) stays in `mounts.rs`; the
      worker (`Mounts`, `run`, `udisks`, `handle`, `list_devices`,
      `ShareListing`, `unmount_share`, `connect`, `list_shares`, `gvfs_root`,
      `gvfs_entries`, `TERMINAL_MOUNT`) moves to `platform/linux/mounts.rs`.
      `icon.rs` (pure pixel code, appendix B block) moves to `src/platform/icon.rs`
      shared by every target, because the macOS drag image needs the same pixels.
      Update `main.rs:10–65`'s `mod` list: `wayland`, `dbus`, `portal` are gone from
      it; `mod platform;` is added. Done when: Linux builds, all moved tests pass
      from their new paths, `tests/portal.rs` gets `#![cfg(target_os = "linux")]`.
      *As built (the code moved on after this plan):* `mounts.rs` now also holds
      the phones (`Phone`, `Protocol`, `phones_from`), the watcher's readers
      (`Change`, `Event`, `Blocks`, `event_from`, `listen`, `restart_due`), `Gio`
      (how `gio` is run, so a test can stand in for it), `Answer`, `Cloud`, and
      `mount_gio`/`unmount_gio`. What moved to `platform/linux/mounts.rs` is what
      talks to the machine: the udisks2 names and `devices_from`,
      `TERMINAL_MOUNT`, `ARRIVAL`, `gvfs_entries`, `GioListing` (was
      `ShareListing`) and `list_shares`, `system_gio`, `gio_error`, `unmount_gio`
      (was `unmount_share`), `mount_gio`, `connect`, the worker's loop `run` with
      `udisks`/`handle`/`list_devices`, and `Monitor` (`gio mount --monitor`).
      Three things stayed portable against the list above: the `Mounts` handle
      (channels, `ask`, `drain`, `in_flight`, the test-only `detached`), whose
      thread now runs `platform::mounts::run` (see S1.24); `gvfs_root`, which is
      now `df_core::du::gvfs_root()` and so df-core's to place; and `listen` and
      `first_line`, pure helpers the Linux worker calls (made `pub`). The card's
      tests took their disks from `devices_from(&objects())`; they now use
      `mounts::tests::disks()`, the same two rows, and a Linux test holds the two
      equal. `icon.rs` moved in S1.22, and `mod platform;` was added in S1.2.
      `wayland` and `dbus` are private to `platform::linux`; `portal` is public
      there for `main`. — done 2dc14d0, Linux verified, other targets unverified
      until CI
- [x] **S1.22** `platform::desktop` (df-app): `pub struct Desktop` with exactly the
      `DataDevice` API (`ready`, `set_selection`, `receive`, `drag`, `poll`) and
      `pub enum Event { Enter, Motion, Leave, Drop, DragEnded, Selection, Copied,
      Pasted }` + `PasteFailure` moved verbatim from `wayland/mod.rs:185–266` into
      `platform/desktop.rs` (portable, no cfg). `pub fn start(event_loop:
      &ActiveEventLoop, window: &Window, waker: Waker) -> Option<Desktop>` is the
      body of `app.rs:2198–2229` (`start_data_device`) moved: Linux matches the
      Wayland raw handles and wraps `wayland::DataDevice`; macOS and Windows return
      `None` in this phase. `app.rs:1510–1513` field type becomes
      `Option<platform::desktop::Desktop>`; `app.rs:12700` uses
      `platform::icon::Rgba`. Done when: Linux behaviour identical (the eight
      `Event` arms in `poll_data_device` compile against the moved enum); the
      "no wayland data device" log line at `app.rs:2192` becomes platform-neutral
      ("no native drag device — drag out and drop in are off"). *As built:* the
      per-target half is each target's `platform/<os>/device.rs` (`Desktop` and
      `start`), which `platform/desktop.rs` re-exports, so `desktop.rs` holds no
      cfg; on Linux `Desktop` is `wayland::DataDevice` itself (`pub use … as
      Desktop`), and on macOS and Windows an empty enum, since `start` never makes
      one. `wayland/icon.rs` moved to `platform/icon.rs` in this task rather than
      S1.21, because `Rgba` is in `Desktop::drag`'s signature. The moved enums'
      doc links to the Wayland thread's private `Command` and `RECEIVE_TIMEOUT`
      now name `Desktop::set_selection`/`receive` and "time"; nothing else in them
      changed. — done 019d4fe, Linux verified, other targets unverified until CI
- [x] **S1.23** `platform::clipboard` (df-app): move the *fallback* transport out of
      `clipboard.rs:330–431` — `copy`, `reap`, `offered_types`, `paste` — into
      `platform/linux/clipboard.rs` (wl-copy/wl-paste bodies unchanged) with stubs
      elsewhere returning `Err(ClipError::Missing("clipboard"))`. Change `copy`'s
      return type to `Result<Option<Child>, ClipError>`: Linux returns `Some(child)`
      (the `--foreground` server the window owns), other targets will return `None`
      once they have a synchronous body. `app.rs:1782–1800` (`WlCopy`),
      `settle_wl_copy`, `retire_wl_copy` handle `None` as "already settled".
      `ClipError::Missing`'s text (`clipboard.rs:322`, "install wl-clipboard") moves
      into the Linux body; the portable text is "no clipboard transport on this
      platform". Everything else in `clipboard.rs` (mime tables, `file_uri`,
      `uri_list`, `Offer`, `choose_offer`, `text_offer`) is portable and stays,
      except `file_uri`/`parse_file_uri` (`:208–261`) which call
      `df_core::platform::os::{as_bytes, from_bytes}` (Phase 3 P3.1) instead of
      `OsStrExt`. Done when: Linux tests in `clipboard.rs` pass; df-app compiles on
      all targets.
      *As built:* `platform/linux/clipboard.rs` holds the four, unchanged but
      for `copy`'s `Ok(Some(child))`, and `missing(tool)`, the "… install
      wl-clipboard" words `ClipError::Missing` now asks the platform for;
      `clipboard.rs` re-exports the four, so the window's calls did not move.
      The macOS and Windows stubs refuse every call with
      `ClipError::Missing("clipboard")`, which reads "No clipboard transport on
      this platform"; their `reap` stops and collects whatever child it is
      handed, though they never hand one out. `Ok(None)` is answered where
      `copy` is called: `copy_via_wl_copy` says the toast at once and keeps no
      `WlCopy`, so `WlCopy`, `settle_wl_copy` and `retire_wl_copy`, which only
      ever hold a running child, did not change. Off Linux's exact bytes,
      `file_uri` spells a name that has none (not valid Unicode, Windows only)
      lossily, and `parse_file_uri` answers `None` for bytes the platform
      cannot spell (not UTF-8, Windows only). — done 7a0999c,
      Linux verified (the stubs type-check when selected on Linux), other
      targets unverified until CI
- [x] **S1.24** `platform::mounts` (df-app): `Mounts::start(notify)`, `ask`, `drain`,
      `connect(url) -> Connected`, `list_shares()`, `TERMINAL_MOUNT: Option<&str>`.
      Linux = the moved worker. Stubs: `ask(Request::List)` answers
      `Reply::Listing { devices: vec![], shares: vec![] }`; every other request
      answers `Reply::Failed("Not available on this platform")`; `connect` returns
      `Connected::Failed(...)`; `TERMINAL_MOUNT` is `None` and `app.rs:6575–6579`
      skips the terminal re-run when it is. `mounts.rs`'s `gvfs_root` is Linux-only
      and moves. The card's "disks empty" text (`Card::disks_empty`) already exists
      for a listing with no rows; the stub uses it. Done when: the `M` card opens
      on every target and shows Places + the connect row; Linux unchanged.
      *As built:* the surface follows the code as it now is. `platform::mounts`
      is `run(requests, replies, notify, gio)` — the body of the worker thread
      that the portable `mounts::Mounts::start(notify, gio)` spawns, so `ask`,
      `drain`, `in_flight` and the test-only `detached` are written once —
      `connect(url, &Gio)`, `mount_gio(root, &Gio)` (a phone),
      `system_gio() -> Gio`, `Monitor` (`start`, `gone`, `started`, `drain`) and
      `TERMINAL_MOUNT: Option<&str>`. The stubs: `run` answers `List` with an
      empty `Listing { devices, phones, shares }` and anything else with
      `Failed("Not available on this platform")`, ringing `notify` for each;
      `connect` and `mount_gio` answer `Connected::Failed` with the same words;
      `system_gio` is a `Gio` that answers `Unsupported`; `Monitor` is an empty
      enum whose `start` is `None`; `TERMINAL_MOUNT` is `None`, and
      `App::connected` toasts that a terminal for the server's questions is not
      available instead of running one. `gvfs_root` did not move: it is
      `df_core::du::gvfs_root()` now (df-core's to place). The card's empty-state
      text is `Card::devices_empty`. The done-when is checked on Linux by
      `app::tests::an_empty_listing_leaves_the_places_and_the_connect_row`, which
      feeds the stub's answer through a detached worker. — done 2c371b5, Linux
      verified (the stubs type-check when selected on Linux), other targets
      unverified until CI
- [x] **S1.25** `format.rs:189–222` (`civil_local`) → `df_core::platform::time::local_civil`
      (S1.10). `app.rs:7201–7255` (`set_mode`) → `df_core::platform::fs::apply_mode`.
      `trashview.rs:163–248` (`row_from`) → `df_core::platform::meta` (S1.7).
      `app.rs:979–982` (`home()`) → `df_core::platform::dirs::home()`.
      `trashview.rs:276–293` (`shorten`) and `finder.rs:336–359` (`shorten_home`)
      take `home()` from the same place. Done when: `grep -rn "std::os::unix\|libc::"
      crates/df-app/src` hits only `platform/`.
      *As built:* `civil_local` keeps its own reading of the seconds (whole
      seconds, the sign kept) and asks `local_civil` for the date, the same
      `localtime_r`; df-core's body checks its conversions, so a year past
      `i32::MAX`, which the old `+ 1900` overflowed, is now `None` and reads as
      unknown — no real mtime is near it. `set_mode` no longer sets a mode
      itself: since the plan was written it goes through
      `df_core::ops::mode::chmod`, S1.19's walk, so there was nothing left to
      route. `row_from` reads `mode`, `uid` and `gid` through
      `platform::meta`; `home()` and `shorten` read `platform::dirs::home()`,
      the same `$HOME` on Linux; `shorten_home` is handed its `home` by callers
      that all use `home()` (its `/` rule is W4.11's). Also routed: the site the
      inventory missed, `app/permissions.rs`'s `refresh_spot_mode`
      (`MetadataExt::mode` → `platform::meta::mode`). With `format.rs` off
      `localtime_r`, df-app's `libc` moved to the
      `[target.'cfg(target_os = "linux")'.dependencies]` table (S1.20's
      deferred half); its only calls are the Wayland thread's. The grep holds
      once S1.26 (`open.rs`) and S1.36 (`cli.rs`) have landed, for everything
      but tests marked `#[cfg(unix)]` or Linux (S1.33). — done 8349438, Linux
      verified (the stubs type-check when selected on Linux),
      other targets unverified until CI
- [x] **S1.26** `platform::open` (df-app): `shell_program`, `shell_argv`,
      `detached_argv`, `which`, `spawn_detached`, `run_blocking` bodies
      (`open.rs:40–134`) move to `platform/linux/open.rs`, re-exported from `open.rs`.
      The Unix body is shared with macOS via `platform/unix/open.rs` **except**
      `detached_argv` (the `setsid --fork` prefix is Linux-only; the macOS body is
      Phase 2 M2.16). `run_blocking`'s `ExitStatusExt::signal` goes through
      `df_core::platform::process::exit_code(&ExitStatus) -> i32`. Windows stub:
      `spawn_detached`/`run_blocking` return `Err(io::Error::from(ErrorKind::Unsupported))`
      (Phase 4 W4.10 supplies the argv model). Done when: `open.rs` tests pass on
      Linux; compiles everywhere.
      *As built:* the shared body is `platform/unix/open.rs` (df-app's first
      `unix` module, `#[cfg(unix)]` in `platform/mod.rs`): `shell_program`,
      `shell_argv`, `command_from`, `spawn_detached` and `run_blocking`, moved
      unchanged but for `run_blocking`'s last line, which is now
      `df_core::platform::process::exit_code(&status)` — added to df-core's
      `platform::process` here, its Unix body the old signal arm moved
      unchanged, its Windows body `status.code().unwrap_or(1)`, with a Unix
      test and a contract row. `detached_argv` and `which` (which only it
      calls) are `platform/linux/open.rs`, which re-exports the shared body;
      macOS re-exports it too, with a `detached_argv` that returns the argv as
      it is — Linux's own answer when `setsid` is missing — until M2.16.
      `open.rs` re-exports `spawn_detached` and `run_blocking`, the two calls
      the window makes, and the contract names only those; the Windows stub
      refuses both with `io::ErrorKind::Unsupported` carrying "Running
      programs is not available on this platform", since the window toasts the
      error's text. The argv model is W4.3's (the W4.10 above is roots and
      fallbacks). The two tests of moved bodies moved with them:
      `paths_reach_the_shell_as_arguments` to `platform::unix::open` and
      `detaching_only_prefixes_what_it_can_find` to `platform::linux::open`
      (S1.33's `/bin/zsh` and `setsid` gates, by where they live). — done
      1a477d3, Linux verified (the stubs type-check when
      selected on Linux), other targets unverified until CI
- [x] **S1.27** `platform::window::attributes(title: &str, app_id: &str) ->
      WindowAttributes`: Linux body is `app.rs:2111–2124` (`with_name` from
      `WindowAttributesExtWayland`); Windows body sets title and size only; macOS
      body sets title and size **and `with_option_as_alt(OptionAsAlt::Both)`**
      (`WindowAttributesExtMacOS`): without it winit delivers Option+p as chord
      `alt+p` *and* `text = "π"`, and an unbound Option+letter types the composed
      character; with it the composed text is stripped and dead-key composition
      (Option+e, e → é) is lost in prompts — the accepted trade-off for a
      keyboard-driven app, logged in `02-macos.md`. `app.rs:init_gfx` calls it.
      Done when: Linux unchanged; compiles everywhere (`winit::platform::wayland`
      and `winit::platform::macos` are not referenced outside `platform/`).
      *As built:* each body builds on `Window::default_attributes()` with the
      title and `app::WINDOW_SIZE` (now `pub(crate)`), so the size stays one
      constant; `app_id` is unused off Linux. `WindowAttributesExtMacOS::
      with_option_as_alt` and `OptionAsAlt::Both` checked against winit 0.30.13's
      `src/platform/macos.rs` (the trait at :280, the method at :304, the enum at
      :518); the macOS body is the one stub that cannot be type-checked on Linux.
      — done 5bb4d59, Linux verified, other targets unverified until CI
- [x] **S1.28** `platform::gfx::PREFERRED_BACKENDS: wgpu::Backends` — Linux `VULKAN`
      (unchanged), macOS `METAL`, Windows `DX12`. `graphics.rs:92` reads it. The
      retry-with-all-backends path stays as is. The four Wayland-reasoning comments
      (`graphics.rs:47–54, 78–90, 99–106, 135–147`) get one added sentence each saying
      what the other platforms do (Metal reports occlusion; Mailbox is absent on
      Metal so Fifo is taken). Done when: compiles everywhere; Linux picks Vulkan as
      before (check the `DF_FRAME_LOG` or the wgpu adapter log line).
      *As built:* also `platform::gfx::PREFERRED_NAME` ("Vulkan", "Metal", "DX12"),
      so the retry's warning names what was tried and reads exactly as before on
      Linux. Checked against wgpu-hal 29.0.4: Metal offers `Fifo` (and
      `Immediate`) but no `Mailbox` (`src/metal/adapter.rs:418`), DX12 always
      offers `Mailbox` (`src/dx12/adapter.rs:1276`); wgpu 29 builds no Vulkan or
      GL backend on Apple without `vulkan-portability`/`angle` (`build.rs`).
      Linux is verified by the code, not a launch (no GUI is driven here): the
      first instance is built from `Backends::VULKAN` exactly as before.
      — done a978d99, Linux verified, other targets unverified until CI
- [x] **S1.29** `platform::fonts::dirs() -> Vec<PathBuf>`: Linux body is
      `icons.rs:72–82` + the `$HOME` additions at `:102–105`; macOS and Windows lists
      are defined in `05-defaults-and-config.md` D5.6 and filled here (they are just
      strings). Done when: `icons.rs` has no path literal.
      *As built:* the one path literal left in `icons.rs` is in its tests, an
      `Entry` fixture's path (`/home/brian/…`), not a font location. D5.6's other
      half — adding `SymbolsNerdFontMono-Regular` to `PREFERRED` — is D5.6's and
      was not done here: it would re-rank a face Linux already accepts as a
      last-resort match. A Linux test pins the order (system directories, then
      `~/.local/share/fonts`, `~/.fonts`). — done 3412429, Linux verified, other
      targets unverified until CI
- [x] **S1.30** `platform::pdfium::{LIBRARY_NAME, candidates()}`: `preview/doc/pdf.rs:41–61`
      builds its list from them. Linux: `libpdfium.so` and the four paths as today.
      macOS: `libpdfium.dylib`, `$DF_PDFIUM_LIB`, `<exe>/../Frameworks/libpdfium.dylib`
      (the bundle), `~/.local/lib/delightfile/libpdfium.dylib`. Windows: `pdfium.dll`,
      `$DF_PDFIUM_LIB`, `<exe dir>\pdfium.dll`. Done when: the pdf.rs test
      `the_library_is_looked_for_in_the_documented_order` is parameterised on the
      platform list and passes on Linux.
      *As built:* `pdf.rs`'s `candidates` moved whole to
      `platform/linux/pdfium.rs`, its three `"libpdfium.so"` joins now joining
      `LIBRARY_NAME` (the same paths, component for component). The pdf.rs test
      asserts what holds on every target — the override first when set, and
      every other candidate named `LIBRARY_NAME` — and its two Linux-only
      assertions (the delightfile and delightviewer per-user copies) moved to
      `platform::linux::pdfium`'s own test. — done df33c49, Linux verified, other
      targets unverified until CI
- [x] **S1.31** CLI: `cli.rs:221, 246–252` produce `Outcome::Portal` only when
      `platform::HAS_PORTAL` (a `const bool`, Linux `true`); otherwise `--portal` is
      an unknown flag. `main.rs:89–91` calls `platform::portal::run()`, which exists
      only on Linux; on other targets the `Outcome::Portal` arm is unreachable and is
      written as `Outcome::Portal => platform::portal::run()` with a stub that
      returns `2` after printing "not available on this platform" (so the match
      stays exhaustive without a cfg in `main.rs`). ~~`main.rs:87` uses
      `std::env::args_os()` with `to_string_lossy` per argument instead of
      `std::env::args()`~~ — already done, and done better, by 7ad55ad: `main`
      hands `cli::parse` the `OsString`s themselves, so a non-UTF-8 path is kept
      byte for byte rather than made lossy (which leaves the Windows half of that
      to S1.36). `USAGE` (`cli.rs:158–189`) is
      assembled from a portable head plus `platform::cli::EXTRA_USAGE` so the
      `--portal` lines appear only on Linux. Done when: `--help` on the macOS
      runner does not mention the portal (assert in a test that builds the string).
      *As built:* `--portal` now also serves `org.freedesktop.FileManager1`
      (2ec71f3), and two other entries name it — `--reveal` ("what Show in folder
      asks --portal for") and `--chooser-request` ("written by --portal") — so
      `USAGE` became `cli::usage() -> String`, assembled from portable pieces and
      three platform ones, `platform::cli::{REVEAL_USAGE, REQUEST_USAGE,
      EXTRA_USAGE}`: Linux's are its lines as they were (the built binary's
      `--help` is byte-identical to the baseline's), the others' are the same two
      entries without the portal and an empty `EXTRA_USAGE`. `--reveal` itself is
      portable and stays everywhere. The match arm is `"--portal" if
      platform::HAS_PORTAL`, so elsewhere `--portal` is an unknown option.
      `cli::tests::the_help_names_the_portal_only_where_there_is_one` asserts
      `help.contains("portal") == HAS_PORTAL`; `portal_stands_alone` asserts the
      unknown option where there is none. — done 1d897c0, Linux verified, other
      targets unverified until CI
- [x] **S1.32** Drop-in via winit on every target: add `WindowEvent::HoveredFile`,
      `HoveredFileCancelled`, `DroppedFile` arms to `app.rs:17066–17162` that push
      `platform::desktop::Event::{Enter, Motion, Leave, Drop}` onto a new
      `App.native_drops: Vec<Event>` drained at the top of `poll_data_device`
      (`app.rs:12724`) before the device's own events. Position: `Enter`/`Motion`
      carry `platform::desktop::pointer_position(&window)` (Linux: the last
      `CursorMoved` position, which is also what a Wayland drag reports; macOS and
      Windows bodies in Phases 2/4), `ours: false` — winit hands over paths only, so
      a drag from another delightfile window is indistinguishable from Finder's and
      lands as a copy; the Linux self-drop semantics (`dnd::SELF_MIME`) are not
      reproduced off-Linux in this plan, logged below. **On Linux this arm never fires**
      (winit does not deliver `DroppedFile` on Wayland), so Linux is unchanged by
      construction; say so in the comment. Done when: a unit test feeds a synthetic
      `DroppedFile` through `App::for_test` and sees a paste into the cwd.
      *As built:* a `&Window` cannot say where the last `CursorMoved` was, so
      `pointer_position` answers `None` on Linux as on the other two for now, and
      the arm falls back to egui's last pointer position (`latest_pos`), which is
      that `CursorMoved`; with neither, nothing is highlighted and the drop lands
      in the folder on screen. winit gives no motion during a drag and one event
      per file, so `App::winit_drop` makes the first `HoveredFile` of a drag
      `Enter` and the rest `Motion`, and gathers a drop's `DroppedFile`s into one
      `Drop`. The queue is read at the top of `poll_data_device`, before the
      device's own early return, by four arms of its own, so the Linux device loop
      is untouched. Under X11 winit *does* deliver these, so an X11 session gains
      drop-in by this path (not a supported session; logged). The unit tests are
      `a_file_dropped_through_winit_is_copied_into_the_folder_on_screen` and
      `winits_hovers_and_drops_read_as_the_devices_do`. — done 415e4ea, Linux
      verified, other targets unverified until CI
- [x] **S1.33** Tests that block df-app's test target from compiling elsewhere
      (appendix B §7 "unix-ext", "unix-socket", "sh"): `#[cfg(unix)]` on
      `app.rs a_click_on_the_spots_space_hint_toggles_the_chosen_bit`, the four
      `app/syncing.rs` tests, `trashview.rs a_trashed_symlink_is_a_symlink…`, the
      `open.rs` tests that assert `setsid`/`/bin/zsh`; `dbus.rs`/`portal/`/`wayland/`
      tests move with their modules. Also `#[cfg(unix)]` on the df-core tests the
      appendix lists as direct Unix API users: `ops/journal.rs` (`PermissionsExt`),
      `fs/tests.rs` (`std::os::unix::fs::symlink`), `state/tests.rs` (`from_vec`),
      and gate `app.rs:17806` (`7z a`), `trashview.rs:535` (`HOME`) and the
      `preview/doc/font.rs:907–918` font-root helper (add the Windows root
      `C:\Windows\Fonts` there instead of gating). Done when: `cargo test -p df-app
      --no-run` succeeds on the macOS and Windows runners.
      *As built:* `#[cfg(unix)]` on `app.rs`
      `an_extraction_here_lands_on_what_the_extractor_made`, the four
      `app/syncing.rs` tests (`a_run_with_problems_comes_back_as_a_card_naming_them`,
      `a_folder_that_could_not_be_read_opens_the_card_and_fails_the_task`,
      `a_socket_is_left_out_in_the_toast_and_is_no_problem`,
      `y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync`, which now
      imports `TEST_SHELL` itself so the module's `use` is not left unused on
      Windows), `trashview.rs a_trashed_symlink_is_a_symlink_and_not_a_special_file`,
      and two the plan predates: `app/tests/undo.rs
      a_copy_redo_that_cannot_land_goes_back_and_undo_works_again`
      (`PermissionsExt`) and `app/tests/tagging.rs
      links_in_a_selection_are_skipped_and_counted` (a symlink).
      `#[cfg(target_os = "linux")]` where a test drives a body only Linux
      has: `app.rs a_click_on_the_spots_space_hint_toggles_the_chosen_bit`
      (its chip goes through the chmod walk, S1.19, so `cfg(unix)` would
      compile on macOS and fail there), the whole of `app/tests/permissions.rs`
      (the same walk, M2.28 un-gates it), `app/tests/trash.rs` (a
      freedesktop trash: `.trashinfo` text, `TRASHINFO_EXT`, `PURGE_STAMP`,
      `Trash::ensure`), `app/tests/appearance.rs` (the portal's fakes, and a
      freedesktop trash), `app/tests/phones.rs` (gvfs, and `ExitStatusExt`),
      and `app/tests/hits.rs undoing_a_trash_brings_the_row_back_unmarked` and
      `redoing_a_trash_takes_the_row_out_again` (they trash through the stub,
      which refuses). The `open.rs` tests moved with their bodies (S1.26);
      `cli.rs`'s is S1.36's. `trashview.rs`'s `HOME` test reads
      `platform::dirs::home()` instead of being gated, the same `$HOME` on
      Linux and a real home elsewhere, and `font.rs`'s helper has
      `C:\Windows\Fonts`. The df-core half was S1.15's. Checked on Linux by
      building df-app's tests with the macOS and then the Windows platform
      modules selected and df-app's own `target_os = "linux"` (and, for
      Windows, `unix`) gates turned off: no errors, and no warning but the
      dead code S1.53 allows; `std::os::unix` and df-core's Linux-only items,
      which such a build still resolves, by grep. — done 3bfad09, Linux
      verified, other targets unverified until CI
- [x] **S1.34** `remote.rs:419–421` uses `df_core::fs::names::suffixed` (moved in
      S1.6). `app.rs:4055–4190` (`show_trash`, `trash_restore`, `trash_purge`) call
      the `platform::trash` surface; on a target where `Trash::home()` is
      `Unsupported`, `App::refusal(Command::ShowTrash)` (and `Trash`) returns the
      message and the key toasts. Done when: the refusal test from S1.6 passes and
      Linux is unchanged.
      *As built:* `free_name` takes `suffixed` and `MAX_TRASH_COLLISIONS` from
      `df_core::fs::names`. The three trash functions already called the
      platform surface through the `ops::trash` shim (S1.6), so they did not
      change; what is new is the refusal, in `App::refusal`: where
      `Trash::home()` is `Err(DfError::Unsupported(_))` — the stub, never Linux,
      whose only `Err` is a missing `$HOME` — `Command::OpenTrash` (the plan's
      `ShowTrash`: `g t`), `Command::EmptyTrash` and `Command::Trash` answer
      "Trash is not available on this platform", so the key toasts it and the
      app menu greys the rows. `Trash` is refused only off a remote tab: `d`
      over the link is a delete on the server, which has no local trash in
      it. The S1.6 test asserts the three refusals and that `d` toasts and
      deletes nothing; compiled with its gate lifted it builds on Linux and
      fails there, as it should. — done 2544733, Linux
      verified, other targets unverified until CI
- [x] **S1.35** `platform::appearance` (df-app): the desktop's light or dark for
      `[flavor] mode = "auto"` (the portal read and its `SettingChanged` watcher,
      which arrived after this plan was written). Move `src/appearance.rs` to
      `src/platform/linux/appearance.rs` with its history — the Linux body
      unchanged: `Desktop` (`watch_over`, `drain`, `heard`, `link`, `started`,
      `wait_first`, and the test-only `fake`/`FakePortal`), `Connect`, `session`,
      the portal names, the match rules, the thread, `fake_bus`, and
      `Scheme::from_value` (the portal's number, kept as an inherent `impl` beside
      the body that reads it). What an answer *means* stays portable in
      `src/appearance.rs`: `Scheme` with `appearance()`, `Link`, `RETRY` (also read
      by `mounts::restart_due`), and the `only_a_clear_light_is_light` test. macOS
      and Windows stubs: `Desktop` starts no thread, answers nothing (which `auto`
      already treats as dark) and reports `Link::Gone` at once, so `theme-auto`
      toasts that it could not reach the setting rather than claiming to follow it;
      `Connect` is `Arc<dyn Fn() -> Result<Infallible, String>>`, a connection
      never made. `app.rs` names `platform::appearance::{Desktop, Connect,
      session}`. Native bodies: M2.30, W4.33. Done when: Linux tests pass from their
      new paths (`platform::linux::appearance::tests`, `app/tests/appearance.rs`),
      Linux behaviour is unchanged, and df-app compiles on the macOS and Windows
      runners with the stubs. — done 631246a, Linux verified (the stubs type-check
      when selected on Linux), other targets unverified until CI
- [x] **S1.36** `cli.rs` reads an argument as bytes (arrived with 7ad55ad, after
      this plan): `parse` asks `OsStrExt::as_bytes(arg).starts_with(b"-")`,
      `flag` splits `--name=value` on the bytes and rebuilds the value with
      `OsStr::from_bytes`, and `a_path_that_is_not_utf8_is_kept_byte_for_byte`
      builds its names with `OsStringExt::from_vec`. It compiles on macOS and not
      on Windows. Second pass, beside S1.23 and S1.25, because the portable
      spelling is df-core's: route both through
      `df_core::platform::os::{as_bytes, from_bytes}` (S1.16, P3.1) — the Linux
      bytes are the same bytes — and give the test `#[cfg(unix)]` (a name that is
      not UTF-8 is a Unix path) or a Windows twin with an unpaired surrogate.
      Done when: `std::os::unix` is gone from `cli.rs`'s non-test code, the `cli`
      tests pass on Linux, and df-app compiles on the Windows runner.
      *As built:* `parse` asks `is_option(&arg)`, `as_bytes(arg)` starting with
      `-`; `flag` strips `--name=` off `as_bytes(arg)` and makes the value with
      `from_bytes`, so `Flag::Value` holds an `OsString` rather than borrowing
      the argument. On Linux and macOS both conversions are the old exact
      bytes. On Windows an argument with no UTF-8 (a lone surrogate) is a path
      rather than an option — an option's name has to be text — and a value
      that could not be spelled would make the argument not that flag (it
      cannot happen: a value cut from valid UTF-8 at an ASCII `=` is valid).
      The non-UTF-8 test is `#[cfg(unix)]`; no Windows twin, since W4.26 owns
      the Windows twins. — done 5a89e18, Linux verified, other
      targets unverified until CI
- [x] **S1.53** Dead code off Linux (added 2026-09-29 at integration). Built
      with the macOS or the Windows platform modules selected, df-app's
      binary has 49 dead-code warnings per target and nothing else: the
      drag-and-drop helpers (`dnd.rs`), gio's output readers and the variants
      only gio and udisks answer in (`mounts.rs`), `platform::desktop::Event`'s
      drag and clipboard variants and `PasteFailure`, the portal's
      `appearance::Scheme` and `Link` states, `clipboard::ClipError::Failed`,
      and the drag image (`platform/icon.rs`) — items whose only callers are
      Linux bodies. Decided: no `allow` attribute in the code and no `cfg`
      outside `platform/` for them. The
      `macos` and `windows` jobs in `.github/workflows/ci.yml` pass
      `-A dead_code` to clippy, each with a comment saying it is removed when
      Phase 2 and Phase 4 give those items callers; that removal is M2.31 and
      W4.34, and `06-build-and-release.md`'s Decisions log says so. The
      `linux` job's clippy line is unchanged. Done when: both foreign jobs'
      clippy lines carry the flag and its comment, the Linux one does not, and
      the workflow still parses. — done 0895136, Linux
      verified (the YAML parses; df-app built and linted on Linux with each
      target's stubs shows only dead code), other targets unverified until CI
- [x] **S1.54** Two tests that failed anywhere but Brian's machine (added
      2026-09-29 at integration; found by running the CI linux job's steps in
      an `archlinux:latest` container, `06-build-and-release.md` Open
      questions). Fixed under `crates/`:
      1. `app::places::tests::the_menus_pin_and_go` took `sub`'s index from
         `DirState::entries()` and handed it to `set_cursor`. `entries()` is
         scan order ("rarely what a caller wants", its doc says) and
         `set_cursor` takes a position on screen, so the cursor landed on
         whichever row the filesystem listed second: `other` on btrfs and
         overlayfs, `sub` on tmpfs. The app never mixes the two: the row
         menu's pin reads `cursor_entry()`, a view position, and no other
         df-app code takes an index from `entries()` for the cursor. So the
         test was wrong, not the app; it now finds its row with
         `cursor_to_name("sub")`. Reproduced before the fix with `TMPDIR` on
         btrfs, and passes after it on btrfs and tmpfs.
      2. `archive::tests::a_zip_written_by_a_real_archiver_lists_the_same_way`,
         built with `bsdtar` where `zip` is missing: `bsdtar -a -cf real.zip .`
         writes `./`, `./one.txt`, `./sub/`, `./sub/two.txt`, and the lister
         counted the `./` member unsafe (a name that normalizes to nothing was
         flagged). Decided: a leading `./` is a harmless spelling of the same
         relative path, so such a member is safe and lists under the name
         without it; `..` components and absolute names stay unsafe.
         `archive::name_is_unsafe` no longer flags a relative name that
         normalizes to the root (`./`, `.`); `./a` and `a/./b` were already
         safe and listed as `a` and `a/b`. The extractor agrees by
         construction: `plan_extract` joins the tree's normalized path onto
         the destination, and `unpack` finds each member by
         `normalize(name)`, so `./one.txt` extracts to `dest/one.txt`, the
         path the listing shows; the `./` member has no row, so it is never
         planned or opened (before, it was counted unsafe but never in the
         plan's skipped list either, so the two disagreed); the external
         extractors (7-Zip, `bsdtar`) extract whole archives with their own
         rules and are never given a member name. New tests:
         `a_dot_component_is_the_same_path_and_only_a_dot_dot_climbs`
         (`./a`, `./`, `./../a`, `a/./b`) and
         `dot_slash_members_list_and_extract_as_the_names_without_it` (lists
         and extracts a zip of `./` members plus `./../escape.txt`, which is
         flagged, skipped and not written). The real-archiver test passes with
         only `bsdtar` on `PATH`.
      The two font tests the container also fails are not this task's (the
      Open question in `06-build-and-release.md` stands). Done when: both
      tests pass with `TMPDIR` on btrfs and without `zip`; Linux's other
      tests unchanged. — done 76411eb, Linux verified

## 4. Closing the phase

- [ ] **S1.40** Remove `continue-on-error` from the macOS and Windows CI jobs
      (`06-build-and-release.md` B6.5). Done when: all three jobs are required and
      green on `main`.
- [>] **S1.41** Update `README.md`'s "Building" section: it currently says "Needs a
      Wayland session"; it now says Linux (Wayland) is the supported platform and
      macOS/Windows builds compile but are unverified, linking to `plans/other-platforms/`. Done
      when: reviewed by Brian.
      — integration session, 2026-09-29: written in e8cc019, and
      waiting on the done-when, Brian's review. The section says the macOS and
      Windows builds are *meant* to compile rather than that they do, since
      no CI run has compiled df-app for either yet (S1.40).
- [x] **S1.42** Update the memory/PLAN pointers: `PLAN.md` gets a one-paragraph
      pointer to `plans/other-platforms/README.md` under a "Cross-platform" heading. Done when: the
      paragraph exists.
      *As built:* `PLAN.md` is gone (removed as done in 6aee8d1), so the
      paragraph went where the repository now points at its plan: the
      README's Status section, whose first line still names `PLAN.md`. It is a
      "Cross-platform" subsection there, one paragraph saying the port is
      planned and marked in `plans/other-platforms/README.md` and that the
      first phase is under way. The memory pointers are outside the
      repository. — done 444804d

## Decisions log

- 2026-09-25 — Whole-file moves for the Linux bodies; shims at the old paths so
  df-app's imports do not churn in Phase 1.
- 2026-09-25 — `UTIME_OMIT` from libc rather than the literal; identical on Linux.
- 2026-09-25 — SFTP and rsync are `Unsupported` on Windows in this phase; SFTP gets
  a Windows body in Phase 4, rsync never does.
- 2026-09-25 — The mechanical byte→UTF-8 rewrites (S1.16) moved here from Phase 3 so
  df-core compiles on Windows at the end of Phase 1.
- 2026-09-25 — egui-winit's `clipboard` and `links` features are turned off (S1.20):
  a Linux-visible change, invisible in behaviour because df-app never consumed
  egui's paste event or opened links through egui.
- 2026-09-25 — Off-Linux, a drop from another delightfile window is an external
  copy; the Linux `SELF_MIME` self-drop semantics are not reproduced.
- 2026-09-29 — df-core's shared Unix bodies are a directory, `platform/unix/mod.rs`,
  not `platform/unix.rs`: S1.12 moves `vfs/poll.rs` to `platform/unix/pipe.rs`, so
  there are several of them. S1.2's file list corrected.
- 2026-09-29 — Stubs that macOS and Windows share live once, in
  `platform/stub/`, selected by `#[cfg(not(target_os = "linux"))]` (the ground
  rules' one use of `not(...)`); `macos/mod.rs` and `windows/mod.rs` each
  `pub use super::stub::<module>`, so a native body lands by swapping that one
  line for `pub mod <module>` and the other target is untouched. Two identical
  copies would drift, and a stub that later diverges moves into its target
  directory then.
- 2026-09-29 — S1.4: `platform::watch::Backend` is `open(control, events, notify)`,
  which starts the thread and returns its handle, plus `wake()`. `set_watches` is
  thread-private state (inotify's wd map), not part of the cross-platform shape,
  so it stays inside the Linux thread body. `Watcher::new` now makes its two
  channels before the backend opens rather than after; channel creation has no
  side effect, so Linux is unchanged. The one Linux-visible change is the
  plan's own: the warning when watching is unavailable reads "directory watching
  unavailable (…)" instead of "inotify unavailable (…)".
- 2026-09-29 — S1.4's done-when named three `Watcher` tests; two start a real
  watcher and are Linux-gated. The third, `a_disabled_watcher_is_inert…`, tests
  the portable disabled watcher and runs on every target.
- 2026-09-29 — S1.5 signatures follow the moved code rather than the sketch:
  `reflink` returns `bool` (it logs and falls back, it never errs),
  `set_times` returns the crate `Result` (a NUL in the path is `DfError::Op`),
  `forget_cached` keeps its `path` for the log line, `same_file` is P3.3's
  `io::Result<bool>`. Of `du/fstype.rs` only `is_remote` and `magic_of` move:
  since the plan was written the file gained `gvfs_root` and `on_device`, which
  are path logic, and `REMOTE_FS_MAGIC`, which `fs::tags::read_on` also reads.
  `sync_dir` keeps its test counters in `ops::copy` and calls the platform for
  the flush. `UTIME_OMIT` is `libc::UTIME_OMIT` as decided; the local constant
  and its doc comment are gone.
- 2026-09-29 — S1.6's stub (`platform/stub/trash.rs`) provides the whole
  surface df-core and df-app call, including the trash-aging functions added
  since the plan: operations refuse with `Unsupported("Trash")`, `list` is
  empty, `purge_due_in` is never due, `available_for` is false. Its
  `TrashedItem` has the shared fields and answers `location()`/`files_path()`/
  `info_path()` with an empty path and `is_orphan()` with `false`.
  `iso8601_utc` and `parse_deletion_date` are real bodies copied into the stub:
  the trash view reads the `DeletionDate` text on every target and M2.8's
  journal writes the same text. What only the freedesktop spec has
  (`.trashinfo` text, `$topdir` trashes, `PURGE_STAMP`, `expired`) is not in
  the stub, so df-app tests that use those items need a Linux gate (S1.33).
- 2026-09-29 — S1.7: `platform::meta` has `mtime` beside P3.4's list, because
  the archive writer stores `st_mtime` in tar and zip headers; its Unix body is
  `MetadataExt::mtime`. `blocks_bytes` is `st_blocks × du::BLOCK_UNIT`, the
  multiplication `du::walk` did, and `du::walk` now reads the apparent size with
  `Metadata::len()` instead of `MetadataExt::size()` — both are `st_size`.
- 2026-09-29 — S1.8, yazi's Windows cache directory: yazi (sxyazi/yazi at
  a228e69) builds its temp cache as `env::temp_dir()` joined with
  `format!("yazi-{}", Uzers::uid_or_zero())` (`yazi-fs/src/xdg.rs`,
  `Xdg::load_temp_dir`), and `uid_or_zero` is `unix_either!(Self::uid(), 0)`
  (`yazi-shim/src/uzers.rs`). So the Windows directory is `%TEMP%\yazi-0`, not
  `yazi-<USERNAME>`: `cache_suffix()` is `"0"` there, and the uid on Unix as
  before. The Linux trash keeps `uid` as a re-export of `platform::user::uid`,
  so `ops::trash::uid` still exists on Linux.
- 2026-09-29 — S1.11: each `platform::dirs` function is the rule its callers
  used, so Linux answers are unchanged: an empty variable counts as unset, a
  relative one is kept, and `runtime_dir()` returns `$XDG_RUNTIME_DIR` as set
  because gvfs wants it absolute and the rclone socket only non-empty — each
  keeps its own test. The freedesktop trash keeps its own
  `$XDG_DATA_HOME`/`$HOME` rule inside `platform/linux/trash.rs`: the spec
  ignores a relative `$XDG_DATA_HOME`, which zoxide's rule (now `data_dir()`)
  does not. `state::state_path_from` stays public and pure (df-app's portal and
  the state tests call it); a unit test pins it to `state_dir()` so the two
  copies of the rule cannot drift.
- 2026-09-29 — S1.13's Windows `candidates` is this task's list,
  `[name.exe, name]` plus `tar.exe`, `tar` for `bsdtar`. W4.2 lists a narrower
  one (`["bsdtar.exe", "tar.exe"]`, no bare names); W4.2 is Phase 4's and
  supersedes this when it lands. The comments in `vfs/tests.rs`,
  `vfs/rclone_tests.rs` and `sync/rsync.rs` that named `libc::kill` and
  `SIGSTOP` in prose were reworded, so the task's grep reads true.
- 2026-09-29 — S1.16, where the `Result` goes when the nearest caller has no
  `DfError` to return: `ops::create`, the archive writer's walk and
  `sync::rsync::remote_digests` propagate it with `?`; `sync::rsync`'s
  `endpoint`, `endpoints`, `dry_run_args` and `run_args` now return `Result`
  (df-app calls none of them; the rsync tests `unwrap`). A record that cannot
  be spelled is skipped: a state-file record or tab, a git status line, an
  rsync itemized or `sha256sum` line. A predicate answers "no"
  (`ops::is_url`, `SyncPlan::is_debris`, the tag walk's hidden check), and
  `ops::trim_trailing_slash` returns the path untrimmed. `fs::names::fit`
  cannot fail (df-app's `suffixed` caller takes an `OsString`), so for a
  non-Unicode name — Windows only — it spells the new name lossily: it is a
  name to create, not a path to find. On Unix every one of these is exactly
  the old byte path, since both conversions are infallible there.
- 2026-09-29 — S1.17: the owner seam is `user_name(uid)` and `group_name(gid)`
  returning `Option<&'static str>`, not a pair of `String`s: `fs::owner`'s
  public functions (df-app calls them) already return `&'static str` out of
  the once-read tables, and an owned pair would allocate per row where the
  paint loop asks. `fs::owner::parse_id_table` stays portable (the passwd test
  parses a table on every target); the file reading and the two caches moved
  to `platform/linux/user.rs`.
- 2026-09-29 — S1.18 (new task): file tags are `user.xdg.tags` extended
  attributes read and written with the Linux `l*xattr` calls, a site the
  inventory predates. Handled by the fixed rule — the Linux body moved
  unchanged into `platform::xattr`, macOS and Windows stubbed — with the macOS
  body left undecided because the attribute to use is a real choice (open
  question in `02-macos.md`). `tags::write` checks `xattr::AVAILABLE` after its
  comma check, so a stub platform refuses with "Tags is not available on this
  platform" rather than the Linux text for a drive that keeps no attributes.
- 2026-09-29 — S1.19 (new task): the permissions change (`C`, `ops/mode.rs`)
  finds each path with `O_PATH | O_NOFOLLOW` descriptors named through
  `/proc/self/fd`, which neither macOS nor Windows has. Fixed rule: the Linux
  walk moved unchanged into `platform::nofollow`, stubs elsewhere refuse
  through the existing `/proc`-missing path (its `ready()` check), so
  `ops::mode::plan` still returns `Ok` with each target's refusal in
  `errors`, as it always has without `/proc`. The disk tests are
  `#[cfg(target_os = "linux")]` because they test the Linux walk; a macOS body
  is M2.28.
- 2026-09-29 — S1.50 (new task): `vfs/child.rs` ties the rclone daemon (and
  df-app's gvfs watcher) to the thread that spawned it with Linux's
  `prctl(PR_SET_PDEATHSIG)`, and stops it with `kill(SIGTERM)`. Fixed rule:
  the file moved whole to `platform/linux/process.rs`, `terminate` on to the
  shared Unix body (macOS has `SIGTERM`), and the tie is a no-op on macOS and
  Windows with a doc comment saying the child can outlive a crash there; a
  parent-exit watcher (M2.29) and a job object (W4.31) are later tasks.
- 2026-09-29 — S1.51 (new task): cloud remotes speak to `rclone rcd` over a unix
  socket. Fixed rule: the POSIX body (connect with timeouts, the private socket
  directory) moved unchanged into the shared `platform::socket`, so macOS keeps
  it; Windows refuses through the vfs spawn error, and its transport is W4.32.
  `platform::socket::Stream` is `UnixStream` on Unix and an uninhabited type
  on Windows, so `vfs::http::post` is written once.
- 2026-09-29 — Two warnings that only a foreign target shows were silenced
  without a cfg outside `platform/`: `fs::owner::parse_id_table` is `pub`
  (only Linux's body calls it outside tests), and `fs::watch::Control::Watch`
  carries `#[allow(dead_code)]` (only a platform's watcher thread reads the
  list, and the stub never starts one).
- (df-app) 2026-09-29 — S1.20: `libc` stays in df-app's `[dependencies]` until
  S1.25 takes `format.rs` off `localtime_r`. Moved now it would stop `format.rs`
  compiling on macOS, where every other `libc`/`std::os::unix` use left in df-app
  still compiles. The lockfile only loses packages (arboard, smithay-clipboard,
  webbrowser and what only they needed). The `image` crate's features on Linux are
  unchanged — arboard asked for `png` there, which df-app already has — but on
  macOS arboard had enabled `image`'s `tiff` decoder, so a TIFF still there goes
  to `ffmpeg_still` instead.
- (df-app) 2026-09-29 — S1.35 added (light mode's `auto` reads the XDG portal over
  D-Bus; it arrived after this plan). The Linux body moved whole; what an answer
  means (`Scheme`, `Link`, `RETRY`) stayed in `crate::appearance`, with
  `Scheme::from_value` — the portal's number — left beside the body that reads it
  as an inherent `impl`. The stubs answer nothing and say `Link::Gone` at once
  rather than delivering a `NoPreference`: the window lands on dark either way,
  and `theme-auto` then says it could not reach the setting instead of claiming
  to follow one. M2.30 and W4.33 carry the native bodies.
- (df-app) 2026-09-29 — S1.21 split `mounts.rs` by what talks to the machine, not
  by the plan's list, which predates the phones: gio's output readers stay
  portable (pure, tested without gio), the processes and the bus move. The
  `Mounts` handle stays portable too — only its thread's body, `run`, is per
  target — because a stub needs the same channels, `in_flight` bookkeeping and
  test-only `detached` as Linux, and three copies of those would drift. The card
  tests' disk fixture became a plain `Vec<Device>` so it exists where
  `dbus::Interfaces` does not; a Linux test pins it to what `devices_from` makes.
- (df-app) 2026-09-29 — S1.31: `--help` is assembled from three platform pieces,
  not one, because `--reveal` and `--chooser-request` name the portal too; Linux's
  pieces are its old lines verbatim, so its `--help` is byte-identical (checked
  against the baseline binary). S1.36 added: `cli.rs`'s byte reads of an argument
  (7ad55ad) do not compile on Windows, and their portable spelling is df-core's
  `platform::os`, so they wait for the second pass rather than growing a second
  copy of that helper here.
- (df-app) 2026-09-29 — S1.32: `pointer_position` is `None` on every target in
  this phase (Linux cannot ask a `&Window` where the last `CursorMoved` was), and
  the caller falls back to egui's last pointer position, which is that
  `CursorMoved` — the plan's Linux answer, reached without a Linux body. The one
  Linux-visible change: winit's X11 backend delivers `DroppedFile`, so an X11
  session (which has no data device) now takes drops as a copy into the folder
  under the pointer; Wayland, the supported session, never sees these events.
- 2026-09-29 — Integration of the df-core and df-app branches: both had appended
  `M2.28` and `W4.31`. The df-core tasks keep their numbers (`M2.28` the macOS
  chmod walk, `M2.29` the parent-exit watch, `W4.31` the job object, `W4.32`
  the rclone transport); df-app's appearance bodies became `M2.30` and `W4.33`,
  in `02-macos.md`, `04-windows.md`, their Decisions logs, S1.35 and the two
  stubs' doc comments. No other ID changed.
- 2026-09-29 — S1.23: `copy`'s `Ok(None)` is answered in `copy_via_wl_copy`
  (the toast at once, no `WlCopy` kept) rather than by giving `WlCopy` an
  optional child: a copy already made has nothing to settle or retire. For a
  name the platform cannot spell (Windows only) `file_uri` is lossy, as
  `fs::names::fit` is (S1.16), and `parse_file_uri` answers `None`, as a
  record that cannot be spelled is skipped. Linux is unchanged: both
  conversions are exact there, and `ClipError::Missing` reads as it did.
- 2026-09-29 — S1.25: df-app's `civil_local` now takes its date from
  df-core's `platform::time::local_civil`, the same `localtime_r`, instead of
  its own copy of the call. One Linux-visible difference, at an input no file
  has: a time whose year is past `i32::MAX` (some 6.7 × 10¹⁶ seconds out),
  where the old `tm_year + 1900` overflowed, is now `None` and the linemode
  shows it as unknown. Every real mtime reads the same numbers as before.
- 2026-09-29 — S1.26: df-app gets a `platform/unix/` module, as the task
  says, for the shell body Linux and macOS share; `which` went with
  `detached_argv` into `platform/linux/open.rs` because nothing else calls
  it, and on macOS it would be dead code. macOS's `detached_argv` returns the
  argv unchanged until M2.16: the child runs parented to the window, what
  Linux already does without `setsid`. df-core's
  `platform::process::exit_code` is Windows `status.code().unwrap_or(1)`, not
  W4.2's `-1` (W4.2's text corrected): a Windows process always has a code,
  and a stand-in should read as a failure the way a shell's non-zero does.
  The Windows opener stub's error carries "Running programs is not available
  on this platform" rather than the bare `Unsupported` kind, whose text
  ("unsupported") is what the window would otherwise toast.
- 2026-09-29 — S1.34: the refusal asks `Trash::home()` whether the error is
  `Unsupported`, so Linux, whose only `Err` there is a missing `$HOME`, never
  sees it. It covers `EmptyTrash` beside the plan's `Trash` and `ShowTrash`
  (which is `OpenTrash`), since emptying a trash the platform does not have
  is the same missing feature, and not `d` on a remote tab, which deletes on
  the server. The words are the stub's `DfError::Unsupported("Trash")` text
  written out, because `App::refusal` answers `&'static str`.
- 2026-09-29 — S1.52 (new task): df-core tests that compile everywhere but
  reach a Linux-only stub (the trash through the journal, the job and the
  mirror) are gated `#[cfg(target_os = "linux")]`, as decided at
  integration, and the task that gives the body (M2.8, W4.7) removes the
  gate. Tests that reach a stub on Windows only (`same_file`, P3.3) are
  left with the Windows path failures to P3.24: that job is red until Phase 3
  either way, and gating them now would hide what P3.3 has to make pass.
- 2026-09-29 — S1.53 (new task): off Linux df-app's items whose only callers
  are Linux bodies are dead code, 49 warnings per target. They are
  allowed by the macos and windows CI jobs (`-A dead_code` on their clippy
  lines), not by `allow` attributes or `cfg`s in the code, and M2.31 and
  W4.34 take the flag off once Phases 2 and 4 have given them callers. The
  linux job still allows none.
- 2026-09-29 — S1.54 (new task), a Linux-visible change: an archive member
  spelled `./` (or `.`), the root of the extraction written relatively, is no
  longer counted unsafe. `bsdtar -cf x.zip .` writes one in every archive it
  makes, so opening such a zip used to say "1 entry cannot be extracted
  safely" about an entry that was never going to be extracted anywhere. A `.`
  component anywhere else was already dropped by `normalize`; `..` and
  absolute names are flagged as before, and nothing that extracts moved. The
  places test's fix is a test fix only: the app was right.
- 2026-09-29 — S1.42: with `PLAN.md` removed (6aee8d1), its "Cross-platform"
  paragraph is a subsection of the README's Status, the one place the
  repository still points at a plan from. The README's link to `PLAN.md`
  itself is left as it is: whether it goes is not this plan's call.
- 2026-09-29 — S1.55 (new task): the private cross-check toolchain has the
  clippy component as well, so df-core is linted for macOS and Windows with
  the CI jobs' own clippy line before a push. The one lint it found was fixed
  in the stub that caused it (`nofollow`'s `Finder` is no longer a unit
  struct), not with an `allow` in `ops::mode`.

## Open questions

(none)
