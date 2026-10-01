# 00 — Ground rules

Status: **done** (this document is rules, not tasks; it is done when it is read)

Read this before any phase document. It fixes the decisions that every phase relies
on, so that four agents working on four phases produce one program.

## 1. Linux does not change

The Linux build is Brian's daily file manager. Nothing in this port may alter what it
does. Concretely:

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` on Linux are
  green before any task is marked done. The pre-existing 34
  `chunks_exact_to_as_chunks` warnings in dv-* and `preview/` are the baseline; no
  new warnings.
- When a phase document says "Linux body unchanged", the code is **moved**, not
  rewritten. Same syscalls, same fallbacks, same error text. A diff of the moved
  function against the original should be whitespace and `use` lines.
- Refactors "for portability" that touch Linux behaviour are not free. They need a
  Decisions-log entry saying what changed on Linux and why it is invisible.
- Test counts on Linux do not go down. A test that moves into a platform module
  keeps running on Linux.

## 2. The platform seam

Two modules, one per crate that has platform code:

```
crates/df-core/src/platform/
    mod.rs        selects the implementation and re-exports it; the API doc lives here
    unix/…        code Linux and macOS share (mode bits, symlink, pipe/poll, localtime, open)
    linux/…       Linux-only bodies (inotify, FICLONE, statfs magics, freedesktop trash)
    macos/…       macOS bodies (kqueue, clonefile, NSFileManager trash, …)
    windows/…     Windows bodies (ReadDirectoryChangesW, Recycle Bin, …)

crates/df-app/src/platform/
    mod.rs
    linux/…       wayland/ and dbus.rs move here, as well as portal/ and mounts' udisks half
    macos/…
    windows/…
```

Rules for the seam:

- **Same function set on every target.** `platform/mod.rs` lists the functions and
  types the rest of the crate may use, with signatures, in a doc table at the top.
  Each `linux/`, `macos/`, `windows/` module implements exactly that set. There are
  no traits and no `dyn`: selection is `#[cfg]` on `mod` declarations plus `pub use`.
  Drift between targets is caught by the CI matrix compiling all three, which is the
  only enforcement and is enough.
- **Callers never see a cfg.** Code outside `platform/` is written once and calls
  `platform::watch::open(...)`, `platform::trash::put(...)` and so on. If you find
  yourself writing `#[cfg(target_os = ...)]` outside `platform/`, the seam is in the
  wrong place. The two exceptions: `#[cfg(unix)]` on a *test* that genuinely tests Unix
  semantics, and `#![windows_subsystem]` in `main.rs`.
- **Stubs are honest.** A target that has no implementation of a feature yet returns
  `Err(DfError::Unsupported("<feature>"))` for operations, `None` for queries, and a
  `Watcher::disabled()`-style inert object for services. Stubs never panic, never
  `todo!()`, never silently succeed. The app turns `Unsupported` into the existing
  refusal path (`App::refusal` / `refuse_where_we_are` → toast), so a missing feature
  reads as "Not available on this platform", not as a crash or as a no-op.
- `DfError` gains one variant: `Unsupported(&'static str)`, message
  `"{0} is not available on this platform"`. That is the only change to `DfError`.
- **cfg spelling**: `#[cfg(target_os = "linux")]`, `#[cfg(target_os = "macos")]`,
  `#[cfg(windows)]`, `#[cfg(unix)]`. Never `#[cfg(not(target_os = "linux"))]` to
  select a real implementation; `not(...)` is only ever used to select a stub, and the
  stub says in its doc comment which targets it stands in for.
- **Where a feature is Linux-only forever** (the file-chooser portal backend, udisks2,
  `gio mount`), the app-level *feature* is gated, not just its body: the CLI flag is
  absent from `--help` on other targets, the menu item does not appear, the keybinding
  refuses with a toast. See each phase document for the exact list.

## 3. Dependency policy

The project's rule is zero new dependencies unless rewriting is impractical. For the
port it is applied like this:

| Target | Allowed additions | Why | Not allowed |
|--------|-------------------|-----|-------------|
| Linux | none | | |
| macOS | `objc2` **0.5**, `objc2-foundation` **0.2**, `objc2-app-kit` **0.2** — the versions winit 0.30.13 depends on (`Cargo.lock` also holds objc2 0.6 / objc2-app-kit 0.3 via another path; do not use those, and note 0.5's macro is `declare_class!`, renamed `define_class!` in 0.6), all under `[target.'cfg(target_os = "macos")'.dependencies]` | winit already compiles them for macOS; pinning to winit's versions means no second copy. Check with `cargo tree -d --target aarch64-apple-darwin` on CI. | `notify`, `trash`, `arboard`, `dirs`, `fsevent`, `core-foundation` (the high-level one), `cocoa`, `objc` (0.2) |
| Windows | `windows-sys`, under `[target.'cfg(windows)'.dependencies]`, at the version winit 0.30.13 uses (`cargo tree -i windows-sys -p winit` names it; `Cargo.lock` has five versions) | winit already compiles it; same version-pin rule | `windows` (the large `implement`-macro crate), `notify`, `trash`, `arboard`, `dirs`, `winapi` |

Note: `arboard` is already in `Cargo.lock` through egui-winit's default `clipboard`
feature, which df-app never uses; S1.20 turns that feature off. It is not an allowed
direct dependency.

One exception under the project's own rule, "rewriting is impractical": **resvg**
0.48 (`text`, `memmap-fonts`), df-app's on macOS and Windows only
(`[target.'cfg(not(target_os = "linux"))'.dependencies]`), to preview an SVG where
the bundled FFmpeg has no librsvg (`04-windows.md` W4.44). Linux builds none of it.

FFI that is a handful of functions (FSEvents, `clonefile`, `SHFileOperationW`,
`ReadDirectoryChangesW`, `GetLogicalDrives`) is declared by hand in the platform
module, the way `fs/inotify.rs` does today, and gets the same treatment: `unsafe`
confined to that file, the three ownership rules from the inotify module essay
restated at the top, and unit tests for the safe wrapper.

`unsafe_code = "warn"` stays at the workspace level; each platform file that needs it
carries `#![allow(unsafe_code)]` with a one-line justification, exactly like
`fs/inotify.rs` and `vfs/poll.rs`.

## 4. Paths

Until Phase 3 lands, df-core assumes Unix paths. After Phase 3:

- Logic uses `std::path` (`components()`, `file_name()`, `parent()`,
  `strip_prefix()`, `join()`), never `as_bytes()` and never splitting on a literal
  `/`. The helpers in `df_core::path` (Phase 3) cover what `std::path` does not:
  the display root, segment lists for the breadcrumb, case-aware comparison.
- **Persisted and wire formats carry paths as UTF-8.** On Unix that is exactly the
  bytes when the path is valid UTF-8, which every real path in Brian's use is. A
  non-UTF-8 path on Unix is still handled on Unix (the byte encoding stays for Linux,
  see §1). On Windows a path that is not valid Unicode (unpaired surrogates) is
  refused by the journal, trash and SFTP layers with `DfError::Unsupported`, never
  mangled.
- Case: Windows and the default macOS volume are case-insensitive. Equality that
  decides something destructive (overwrite prompts, "is this the same file") goes
  through `platform::fs::same_file` (device+inode on Unix, file index on Windows),
  never string comparison.

## 5. Processes

- The Unix targets keep shell-string openers (`command = 'zeditor "$@"'`) run through
  `$SHELL -c` exactly as today. Windows openers are argv lists; Phase 4 defines the
  config shape and Phase 5 the defaults.
- Every spawn goes through `platform::process` for the parts that differ: detach from
  the window (`setsid` on Linux, `process_group(0)` on macOS,
  `CREATE_NEW_PROCESS_GROUP` on Windows — **not** `DETACHED_PROCESS`, which would
  deny a console to `wt`/`cmd` openers that need one), kill (`SIGTERM` vs
  `TerminateProcess`), console suppression (`CREATE_NO_WINDOW` on Windows for the
  *tools* the app runs headlessly — `git`, `7z`, `ssh`, `fd`, `rg` — so none of them
  flashes a console; never for openers).
- Binaries the app shells out to and their per-target availability are tabled in
  `05-defaults-and-config.md` §4. Missing binaries are already "missing feature, never
  a failure" on Linux; the same rule applies everywhere.

## 6. Testing

- Platform-specific tests live in the platform module under the same cfg as the code.
- Portable tests use `std::env::temp_dir()` or the `TempTree` fixture
  (`df_core::test_support`), never a literal `/tmp`. Phase 1 includes a sweep of the
  existing test files for this (`appendix-inventory-df-core.md` §5,
  `appendix-inventory-df-app.md` §7 list them).
- A test that needs Unix semantics (mode bits, `getuid`, symlink without privilege,
  `EXDEV`) gets `#[cfg(unix)]`. A test that needs a binary (`git`, `ssh`, `7z`) already
  skips when it is absent; keep that.
- **Local cross-compilation on Brian's machine**: the system toolchain is Arch's
  `rust` package and stays untouched; it builds and tests Linux, with the
  worktree's own `target/`. A private rustup (1.98.1, the same version as the
  system `rust`, with clippy) with both foreign targets lives out of the way
  under `~/.cache/delightfile-xcheck`, and is used **only** for cross checks,
  through `build/xcheck.sh`:
  ```
  build/xcheck.sh aarch64-apple-darwin clippy -p df-core -p df-app --all-targets \
      -- -D warnings -A clippy::chunks_exact_to_as_chunks -A dead_code
  build/xcheck.sh x86_64-pc-windows-msvc clippy -p df-core -p df-app --all-targets \
      -- -D warnings -A clippy::chunks_exact_to_as_chunks -A dead_code
  ```
  The first argument is the target and the rest a cargo command, which gets
  `--target` after its subcommand; `check` and `clippy` are the ones that mean
  anything. The script sets `RUSTUP_HOME`, `CARGO_HOME` and a
  `CARGO_TARGET_DIR` of its own (`~/.cache/delightfile-xcheck/target-<target>`)
  for its one cargo and never for the shell, and when the toolchain or the
  target is missing it says so and prints the `rustup-init` (or `rustup
  target add`) command that makes it. df-core has no C dependencies, so it
  needs nothing else. df-app has two build scripts that want the target's C
  toolchain — `ffmpeg-sys-next` runs bindgen over the FFmpeg headers, and
  `libsqlite3-sys` compiles its bundled SQLite — and `check` needs neither's
  output to be right, only to exist, so the script writes stand-ins under
  `~/.cache/delightfile-xcheck/stand-ins` on every run: an `FFMPEG_DIR`
  whose `include` links the host's FFmpeg 9 header folders; bindgen told to
  read them as a clang target with the real target's C data model (x86_64
  Linux for macOS, LP64 either way; MSVC itself for Windows, with a handful
  of C library headers the script writes, since glibc's only know LP64 and
  Windows' `long` is 32 bits); and a `CC` and `AR` for the target that write
  an empty object and an empty archive. Both crates' `check` and `clippy
  --all-targets` then type-check for either target, the objc2 and
  windows-sys calls included. Only the runners build, link and run
  (`06-build-and-release.md`).
- Nothing about the macOS or Windows *UI* can be verified from Linux. Every task
  that changes on-screen behaviour on those targets is marked done at "compiles and
  unit tests pass" and is listed in `07-verification.md` for a human pass. Do not
  claim a gesture works on a platform nobody has run it on.

## 7. Working practice

- Implementers work in a worktree branched from `main`, rebase before handing back,
  and do not commit to `main`. Brian reviews and commits.
- Match the codebase's voice: module-level essays that say *why*, function docs that
  say what a caller needs to know, no comments that restate the code.
- The repository's naming and style conventions apply (rustfmt, clippy-clean,
  `unwrap_used` warn).
- When a phase document and the code disagree (a function was renamed since the
  document was written), fix the document in the same change and note it in the
  Decisions log. The document is the source of truth only while it is true.

## Decisions log

- 2026-09-25 — No traits for the platform layer; `cfg`-selected modules with a fixed
  function set. Reason: three implementations, one caller, and the CI matrix already
  enforces the shape. A trait would add a type parameter or a `dyn` to every caller
  for nothing.
- 2026-09-25 — Hand-rolled FFI over the `notify`/`trash`/`arboard` crates. Reason: the
  project's dependency rule, and each binding is a few functions; the winit-shared
  `objc2`/`windows-sys` crates are the one allowed addition because they cost no new
  compilation.
- 2026-09-25 — macOS before Windows. Reason: macOS compiles most of df-core as is and
  proves the seam; Windows needs the Phase 3 path work first.
- 2026-09-25 — Process-per-window stays on all platforms. Reason: it is how
  cross-window drag works and how the code is shaped; one Dock icon per window on
  macOS is accepted. Revisit only if a macOS user pass finds it unbearable.
- 2026-09-29 — Local cross checks use a private rustup under
  `~/.cache/delightfile-xcheck` (its own `RUSTUP_HOME`, `CARGO_HOME` and
  `CARGO_TARGET_DIR`), not `pacman -S rustup`. Reason: replacing Arch's `rust`
  with rustup would change the toolchain every Linux build uses; a side
  toolchain at the same version checks the foreign targets and leaves the daily
  build exactly as it was (§6).
- (df-app) 2026-09-29 — df-app *is* cross-checked for macOS locally, with
  stand-ins for the two C build steps (§6). Reason: a runner round trip is
  twelve minutes, and every AppKit call Phase 2 makes is otherwise first
  compiled there; the check proves the types, the runner still proves the
  link and the behaviour.
- 2026-09-29 — The cross check is a committed script, `build/xcheck.sh
  <target> <cargo args…>`, for macOS and Windows alike (Brian's call): the
  df-app branch's scratch copy named a session directory and was partly
  lost. It keeps everything it makes under `~/.cache/delightfile-xcheck`,
  names no path outside it and the repository, and says how to make the
  toolchain when it is not there (§6). Windows needed more than macOS did:
  read as x86_64 Linux, glibc's `long` fields gave bindgen layouts that
  Windows' 32-bit `c_long` fails at compile time, so the Windows run reads the
  FFmpeg headers as MSVC over a few C library headers of the script's own.
- (polish) 2026-10-01 — resvg is allowed off Linux (§3): an SVG renderer —
  XML and CSS, paths, gradients and filters rasterised, text shaped — is
  what "rewriting is impractical" means, and the FFmpeg the Windows zip and
  the macOS app carry has no librsvg, so without it an SVG there was "no
  decoder". 28 crates join each of those two trees, 24 of them new to
  `Cargo.lock`; Linux's tree is unchanged (`04-windows.md` Decisions log,
  W4.44).

## Open questions

(none)
