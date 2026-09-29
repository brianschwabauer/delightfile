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
      — done 1758eda
- [>] **S1.2** Create `crates/df-core/src/platform/{mod.rs, unix/mod.rs, linux/mod.rs,
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
      — df-core core-seam session, started 2026-09-29: df-core half done 1758eda;
      the df-app half (§3) and the B6.1 workflow belong to other agents.
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
      hits only `platform/`. — done 66e285d; the grep holds from fdbf5ec, once
      S1.18 (`fs/tags.rs`), S1.19 (`ops/mode.rs`'s descriptor walk) and S1.50
      (`vfs/child.rs`) had moved the last hits.

## 2. df-core: filesystem primitives

- [>] **S1.4** `platform::watch`: move `fs/inotify.rs` (whole file) and the run loop
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
      compiles on all three targets.
- [>] **S1.5** `platform::fs` (df-core), moving these bodies out of `ops/copy.rs`,
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
      — df-core core-seam session, started 2026-09-29; the grep's other hits are
      later tasks' sites (S1.7, S1.12, S1.13, S1.15, S1.16, S1.18–S1.51).
- [>] **S1.6** `platform::trash`: `git mv crates/df-core/src/ops/trash.rs
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
      — df-core core-seam session, started 2026-09-29: df-core half done d7768c1;
      the df-app refusal test is the §3 agent's, with S1.34.
- [>] **S1.7** `platform::meta` — the `MetadataExt` surface is specified in
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
      outside `platform/` in df-core, Linux tests pass. — df-core core-seam
      session, started 2026-09-29; the remaining hits are tests (S1.15) and
      `vfs/rclone.rs` (S1.51).
- [x] **S1.8** `platform::user`: `uid() -> u32` (Unix `getuid`; Windows: `0`, and
      **`cache_suffix() -> String`** used by `preview/cache.rs:128–132, 195–212` for
      `yazi-<suffix>`: Unix the uid, Windows ~~the `USERNAME` env var~~ `"0"` — yazi's
      own Windows convention, confirmed in its source, see the Decisions log). Callers: `ops/trash.rs:588–594`
      (moved), `sync/mod.rs:431, 443–458` (moved), `preview/cache.rs`, and
      df-app `mounts.rs:gvfs_root` (Linux-only after S1.20), and since the plan was
      written df-core's own `du::gvfs_root` and `vfs/rclone.rs`'s socket directory.
      Done when: no `libc::getuid` outside `platform/`. — done ab0c2bb,
      cross-checked locally, CI pending
- [x] **S1.9** `platform::thread::lower_priority(nice: i32) -> bool`: Linux body from
      `thread.rs:27–43`; the macOS body (a QoS class via
      `pthread_set_qos_class_self_np`, M2.5) is **not** done in this phase because it
      changes scheduling class rather than niceness and needs a live check; macOS and
      Windows stub no-op.
      Callers unchanged (they call `df_core::thread::lower_priority`, which now
      delegates; the clamp to 1–19 stays there, on every target). Done when:
      compiles on all targets. — done 9419fde, cross-checked locally, CI pending
- [x] **S1.10** `platform::time::local_civil(secs: i64) -> Option<Civil>` with the body
      of `rename/facts.rs:53–88`: Unix `localtime_r`; Windows `localtime_s` (present
      in the windows libc). Also df-app's `format.rs` localtime (appendix B) calls
      the same function. Done when: `Civil::local` tests pass on Linux; compiles on
      Windows. — done bf77db9, cross-checked locally, CI pending
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
      `platform::dirs`, Linux tests pass. — done 865e921, cross-checked locally,
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
      0f42ec2, cross-checked locally, CI pending
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
      `platform/`; archive tests pass on Linux. — done 283a965; the grep holds
      from fdbf5ec, which moved `vfs/child.rs`.
- [x] **S1.14** `sync::rsync::available()` (`sync/rsync.rs:69–77`) returns `false` on
      Windows without spawning (rsync is not a Windows tool; the `host:` endpoint
      syntax collides with drive letters). macOS keeps the real check; the version
      gate is Phase 2. Done when: a Windows-shaped unit test of the gating function
      exists (pure) and the function has no cfg outside `platform/` (put the
      constant `platform::process::HAS_RSYNC: bool` in the platform module).
      — done d9436e3, cross-checked locally, CI pending (the pure gate is
      `sync::rsync::gated(has_rsync, probe)`; its test runs on every target)
- [>] **S1.16** Bytes ↔ UTF-8 at every `OsStrExt`/`OsStringExt` site outside
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
      — df-core core-seam session, started 2026-09-29; the grep's last hits are
      `fs/tags.rs` (S1.18) and `state/tests.rs` (S1.15).
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
      df-core outside `platform/`. — done d9af4f4
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
      `sync/tests.rs`'s inode check (`platform::meta::ino`). — done SHA_S115,
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
      — done cb54c3a, cross-checked locally, CI pending
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
      — done 1dbe892, cross-checked locally, CI pending
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
      compiles on all three targets. — done fdbf5ec, cross-checked locally, CI
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
      df-core compiles on all three targets. — done d9f34ca, cross-checked
      locally, CI pending

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

- [ ] **S1.20** Cargo: move `wayland-client` from `[dependencies]` to
      `[target.'cfg(target_os = "linux")'.dependencies]` in `crates/df-app/Cargo.toml`
      (the workspace entry at `Cargo.toml:37` stays). Move `libc` to the same Linux
      target table: after S1.25 its remaining df-app uses are the nine in
      `wayland/mod.rs` (`pipe2`, `poll`, `read`, `write`, `memfd_create`), all Linux.
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
      aarch64-apple-darwin` (on CI) shows no `wayland-*`.
- [ ] **S1.21** `git mv crates/df-app/src/wayland crates/df-app/src/platform/linux/wayland`,
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
- [ ] **S1.22** `platform::desktop` (df-app): `pub struct Desktop` with exactly the
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
      ("no native drag device — drag out and drop in are off").
- [ ] **S1.23** `platform::clipboard` (df-app): move the *fallback* transport out of
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
- [ ] **S1.24** `platform::mounts` (df-app): `Mounts::start(notify)`, `ask`, `drain`,
      `connect(url) -> Connected`, `list_shares()`, `TERMINAL_MOUNT: Option<&str>`.
      Linux = the moved worker. Stubs: `ask(Request::List)` answers
      `Reply::Listing { devices: vec![], shares: vec![] }`; every other request
      answers `Reply::Failed("Not available on this platform")`; `connect` returns
      `Connected::Failed(...)`; `TERMINAL_MOUNT` is `None` and `app.rs:6575–6579`
      skips the terminal re-run when it is. `mounts.rs`'s `gvfs_root` is Linux-only
      and moves. The card's "disks empty" text (`Card::disks_empty`) already exists
      for a listing with no rows; the stub uses it. Done when: the `M` card opens
      on every target and shows Places + the connect row; Linux unchanged.
- [ ] **S1.25** `format.rs:189–222` (`civil_local`) → `df_core::platform::time::local_civil`
      (S1.10). `app.rs:7201–7255` (`set_mode`) → `df_core::platform::fs::apply_mode`.
      `trashview.rs:163–248` (`row_from`) → `df_core::platform::meta` (S1.7).
      `app.rs:979–982` (`home()`) → `df_core::platform::dirs::home()`.
      `trashview.rs:276–293` (`shorten`) and `finder.rs:336–359` (`shorten_home`)
      take `home()` from the same place. Done when: `grep -rn "std::os::unix\|libc::"
      crates/df-app/src` hits only `platform/`.
- [ ] **S1.26** `platform::open` (df-app): `shell_program`, `shell_argv`,
      `detached_argv`, `which`, `spawn_detached`, `run_blocking` bodies
      (`open.rs:40–134`) move to `platform/linux/open.rs`, re-exported from `open.rs`.
      The Unix body is shared with macOS via `platform/unix/open.rs` **except**
      `detached_argv` (the `setsid --fork` prefix is Linux-only; the macOS body is
      Phase 2 M2.16). `run_blocking`'s `ExitStatusExt::signal` goes through
      `df_core::platform::process::exit_code(&ExitStatus) -> i32`. Windows stub:
      `spawn_detached`/`run_blocking` return `Err(io::Error::from(ErrorKind::Unsupported))`
      (Phase 4 W4.10 supplies the argv model). Done when: `open.rs` tests pass on
      Linux; compiles everywhere.
- [ ] **S1.27** `platform::window::attributes(title: &str, app_id: &str) ->
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
- [ ] **S1.28** `platform::gfx::PREFERRED_BACKENDS: wgpu::Backends` — Linux `VULKAN`
      (unchanged), macOS `METAL`, Windows `DX12`. `graphics.rs:92` reads it. The
      retry-with-all-backends path stays as is. The four Wayland-reasoning comments
      (`graphics.rs:47–54, 78–90, 99–106, 135–147`) get one added sentence each saying
      what the other platforms do (Metal reports occlusion; Mailbox is absent on
      Metal so Fifo is taken). Done when: compiles everywhere; Linux picks Vulkan as
      before (check the `DF_FRAME_LOG` or the wgpu adapter log line).
- [ ] **S1.29** `platform::fonts::dirs() -> Vec<PathBuf>`: Linux body is
      `icons.rs:72–82` + the `$HOME` additions at `:102–105`; macOS and Windows lists
      are defined in `05-defaults-and-config.md` D5.6 and filled here (they are just
      strings). Done when: `icons.rs` has no path literal.
- [ ] **S1.30** `platform::pdfium::{LIBRARY_NAME, candidates()}`: `preview/doc/pdf.rs:41–61`
      builds its list from them. Linux: `libpdfium.so` and the four paths as today.
      macOS: `libpdfium.dylib`, `$DF_PDFIUM_LIB`, `<exe>/../Frameworks/libpdfium.dylib`
      (the bundle), `~/.local/lib/delightfile/libpdfium.dylib`. Windows: `pdfium.dll`,
      `$DF_PDFIUM_LIB`, `<exe dir>\pdfium.dll`. Done when: the pdf.rs test
      `the_library_is_looked_for_in_the_documented_order` is parameterised on the
      platform list and passes on Linux.
- [ ] **S1.31** CLI: `cli.rs:221, 246–252` produce `Outcome::Portal` only when
      `platform::HAS_PORTAL` (a `const bool`, Linux `true`); otherwise `--portal` is
      an unknown flag. `main.rs:89–91` calls `platform::portal::run()`, which exists
      only on Linux; on other targets the `Outcome::Portal` arm is unreachable and is
      written as `Outcome::Portal => platform::portal::run()` with a stub that
      returns `2` after printing "not available on this platform" (so the match
      stays exhaustive without a cfg in `main.rs`). `main.rs:87` uses
      `std::env::args_os()` with `to_string_lossy` per argument instead of
      `std::env::args()`, which panics on a non-Unicode argument (a real risk on
      Windows, harmless on Linux). `USAGE` (`cli.rs:158–189`) is
      assembled from a portable head plus `platform::cli::EXTRA_USAGE` so the
      `--portal` lines appear only on Linux. Done when: `--help` on the macOS
      runner does not mention the portal (assert in a test that builds the string).
- [ ] **S1.32** Drop-in via winit on every target: add `WindowEvent::HoveredFile`,
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
- [ ] **S1.33** Tests that block df-app's test target from compiling elsewhere
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
- [ ] **S1.34** `remote.rs:419–421` uses `df_core::fs::names::suffixed` (moved in
      S1.6). `app.rs:4055–4190` (`show_trash`, `trash_restore`, `trash_purge`) call
      the `platform::trash` surface; on a target where `Trash::home()` is
      `Unsupported`, `App::refusal(Command::ShowTrash)` (and `Trash`) returns the
      message and the key toasts. Done when: the refusal test from S1.6 passes and
      Linux is unchanged.

## 4. Closing the phase

- [ ] **S1.40** Remove `continue-on-error` from the macOS and Windows CI jobs
      (`06-build-and-release.md` B6.5). Done when: all three jobs are required and
      green on `main`.
- [ ] **S1.41** Update `README.md`'s "Building" section: it currently says "Needs a
      Wayland session"; it now says Linux (Wayland) is the supported platform and
      macOS/Windows builds compile but are unverified, linking to `plans/other-platforms/`. Done
      when: reviewed by Brian.
- [ ] **S1.42** Update the memory/PLAN pointers: `PLAN.md` gets a one-paragraph
      pointer to `plans/other-platforms/README.md` under a "Cross-platform" heading. Done when: the
      paragraph exists.

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

## Open questions

(none)
