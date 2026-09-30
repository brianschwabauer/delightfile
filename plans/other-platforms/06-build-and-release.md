# 06 — Build and release

Status: **in progress**

Scope: the CI matrix that keeps all three targets compiling, the release workflow
that produces installable artifacts, the packaging of native libraries the app needs
at runtime (FFmpeg, pdfium), distribution channels (Homebrew tap, scoop), and the
signing decision. This document runs alongside Phase 1: the CI job (§1) must exist
before Phase 1 can be marked done, because it is the only enforcement of the platform
seam. Everything from §3 on waits for Phase 2 (macOS) or Phase 4 (Windows) to have
something worth shipping.

When this document was written there was no `.github/` directory, no CI, and no
release process: Brian builds with `cargo build --release` and `build/install.sh`
(see `appendix-inventory-df-app.md` §5 for what that script does). Since 2026-09-29
`.github/workflows/ci.yml` holds the §1 matrix, green on all three targets on
`main` since run 36669749789 (39c4d51). Since 2026-09-30
`.github/workflows/release.yml` holds §4, and `build/<os>/` the §3 packaging; no
release has been tagged yet.

## Decisions already made

- **Targets**: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
  `x86_64-pc-windows-msvc`. No Intel macOS and no ARM Windows until someone asks.
  GitHub's `macos-14` and later runners are Apple Silicon; `macos-13` is Intel and is
  not used.
- **Linux CI runs in an Arch container**, not on Ubuntu. Reason: `ffmpeg-next` 9 is
  pinned to FFmpeg 9 / libavcodec 63, which Ubuntu LTS does not ship, and Brian's
  machine is Arch, so the container reproduces the one environment the app is known
  to work in.
- **Not signing, initially.** Neither macOS notarization nor Windows Authenticode.
  Distribution is a Homebrew tap and a scoop bucket plus GitHub Releases, with the
  unsigned-app instructions in the README. §6 has the full reasoning and §7 the
  optional signing workflow for when that changes. This decision is Brian's to
  reverse, not an agent's.
- **Bundle FFmpeg and pdfium in the macOS and Windows artifacts.** Users on those
  platforms have no package manager that reliably provides FFmpeg 9 shared libraries,
  and pdfium is not packaged anywhere. Linux stays as today (system FFmpeg, pdfium
  optional from `~/.local/lib/delightfile/`).
- **Release artifacts are built on the runners, not cross-compiled.** Cross-compiling
  FFmpeg-linked binaries from Linux is possible with osxcross and mingw but not worth
  the maintenance; the runners are free for a public repository.
- **Version** comes from `Cargo.toml`'s `[workspace.package] version`. A release is a
  tag `v<version>`; the release workflow refuses to run if the tag and the crate
  version disagree.

## 1. CI matrix (do with Phase 1)

- [x] **B6.1** Create `.github/workflows/ci.yml` triggered on `push` to `main` and on
      `pull_request`. Jobs: `linux`, `macos`, `windows`. Each job: checkout, toolchain,
      native deps (§2), `cargo build --workspace`, then tests: on Linux `cargo test
      --workspace`; on macOS and Windows `cargo test -p df-core -p df-app` only,
      because the dv-* test suites spawn `ffmpeg`/`bash build/test-assets.sh`/`espeak`
      and hold `/home/brian/…` literals (appendix B §6) and are not part of the port
      (the vendored crates build there, which `cargo build --workspace` proves);
      `cargo clippy --workspace --all-targets -- -D warnings` with the 34 baseline
      `chunks_exact_to_as_chunks` warnings allowed via
      `-A clippy::chunks_exact_to_as_chunks` until they are fixed separately, then
      `target/<profile>/delightfile --version` as a load-and-run smoke test.
      Done when: the workflow file exists and the `linux` job is green on `main`.
      The `macos` and `windows` jobs are allowed to fail until Phase 1's stubs land
      (`continue-on-error: true` with a comment naming the Phase 1 task that removes
      it). — written 2026-09-29, linux job's steps pass in a local archlinux
      container except `cargo test --workspace` (4 failures, Open questions),
      unverified on GitHub until the first push. The macOS and Windows jobs run
      `cargo build -p df-core`, `cargo test -p df-core` and `cargo build -p dv-media`
      ahead of the B6.1 steps, so that while they fail the log shows how far each
      gets (B6.3's, B6.4's and B6.7's done-when). The linux job's cargo steps run as
      an unprivileged `builder` user, because the container's root ignores
      permission bits (Decisions log).
      — done 9afe895, verified on the runner (2026-09-30 note): the linux job is
      green on `main` in run 36669749789 (39c4d51), with the macOS and Windows jobs.
- [x] **B6.2** Linux job runs in `container: archlinux:latest` with
      `pacman -Syu --noconfirm rust ffmpeg pkgconf clang git`. Cache
      `~/.cargo/registry`, `~/.cargo/git`, `target/` keyed on `Cargo.lock` and the
      job name.
      Done when: a cold run and a warm run both pass and the warm run is under 10
      minutes. — written 2026-09-29, pacman line and cache hand-over pass in a local
      archlinux container, unverified on GitHub until the first push. The key is
      `<job>-rustc-<hash of rustc -vV>-<hash of Cargo.lock>`; the compiler is in it
      because Arch's `rust` rolls (Decisions log). `~` here is the build user's home,
      so the cached cargo directories are `/home/builder/.cargo/{registry,git}`.
      Neither the cold time on a GitHub runner nor the warm one is known yet.
      — done 9afe895, verified on the runner (2026-09-30 note): cold, run
      36647706325's linux job missed the cache and passed in 14 min 57 s; warm, run
      36669749789's restored it and passed in 7 min 10 s.
- [x] **B6.3** macOS job on `macos-latest` (verify it is arm64 with `uname -m` in the
      log). Toolchain via `dtolnay/rust-toolchain@stable`. Native deps per §2.2.
      Done when: `cargo build -p df-core` and `cargo test -p df-core` pass on the
      runner. (df-app follows once Phase 1 stubs exist.) — written 2026-09-29,
      unverified until the first push. `uname -m` is logged and the step fails if it
      is not `arm64`; the action is pinned by commit with `toolchain: stable`.
      — done 9afe895, verified on the runner (2026-09-30 note): run 36669749789's
      macOS job, arm64, builds and tests df-core and the rest of B6.1.
- [x] **B6.4** Windows job on `windows-latest`, MSVC toolchain. Native deps per
      §2.3. Same done-when as B6.3. — written 2026-09-29, unverified until the first
      push. Toolchain `stable-x86_64-pc-windows-msvc`; `core.autocrlf false` before
      checkout. — done 9afe895, verified on the runner (2026-09-30 note): run
      36669749789's Windows job builds and tests df-core and the rest of B6.1.
- [x] **B6.5** Remove `continue-on-error` from the macOS and Windows jobs. This is the
      last task of Phase 1 (`01-platform-seam.md` S1.40 cross-references it).
      Done when: all three jobs are required and green on `main`.
      — done 0e52895, verified on the runner: CI run 36729161540 on `port/release`
      has the three jobs green with neither allowed to fail. Making them required
      checks on `main` is a branch-protection setting only Brian can make
      (Decisions log, 2026-09-30).
- [x] **B6.6** Add `cargo fmt --check` to the linux job only. Done when: green.
      — written 2026-09-29, passes in a local archlinux container, unverified on
      GitHub until the first push. It is the first cargo step in the job, straight
      after the ownership hand-over. — done 9afe895, verified on the runner
      (2026-09-30 note): green in run 36669749789.

**Local run of the linux job, 2026-09-29.** Its steps ran in order in Docker's
`archlinux:latest`, generated from `ci.yml` itself. The checkout was a clone of a git
bundle of the branch, the cache a cold miss, and the run was capped at 8 CPUs with
`CARGO_BUILD_JOBS=8`. That day pacman gave rust 1.98.1 and ffmpeg 2:9.0.2-1
(libavcodec 63.1.102). Results:

| Step | Result |
|---|---|
| pacman line, user, checkout, versions, ownership hand-over | pass (pacman 24 s) |
| `cargo fmt --check` | pass |
| `cargo build --workspace` | pass, 431 s cold |
| `cargo test --workspace` | fail (4 real failures, below) |
| `cargo clippy …` | pass, 105 s |
| `delightfile --version` | pass |

Test counts with `--no-fail-fast`:

- df-app: 1216 passed, 3 failed, plus the 2 `tests/portal.rs` tests.
- df-core: 1130 passed, 1 failed, 1 ignored.
- dv-core: 167 passed.
- dv-media: 31 unit tests, plus 21 + 1 + 1 integration tests.
- dv-playback: 60 unit tests, plus 2 in `mix_render`.
- df-core doctests: 1.

The four failures fail again when run alone, so they are not load flakes; Open
questions has them. The 12 permission tests the builder user is there for all
passed. A second pass chowned `target/` and the cargo home to root, the way a
restore could leave them, then ran the hand-over, the build and `--version` again.
All three passed, and cargo rebuilt nothing.

Skipped in the container and run on the host, each for a missing binary or file:

- rsync: 9 df-core `sync::rsync` tests and df-app `y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync`.
- 7z: 6 df-core `archive` tests, df-app `an_extraction_here_lands_on_what_the_extractor_made`, and df-app
  `preview::listing::tests::{a_7z_lists_through_7zip, a_tar_bz2_lists_its_members_through_7zip}`, which
  return without a message.
- rclone: 8 df-core `vfs::rclone*` tests.
- sftp-server: 2 df-core `vfs::tests`.
- No system font: 5 df-app `preview::doc::font` tests.
- No yazi cache: `preview::cache::tests::opportunistically_matches_a_real_yazi_entry`.
- No zoxide database: `zoxide::tests::the_real_database_parses_if_it_exists`.

git was installed, so nothing skipped for it.

## 2. Native dependencies per target

What every target has to supply, read from the repository on 2026-09-29:
`Cargo.lock` holds `ffmpeg-next` 9.0.0 and `ffmpeg-sys-next` 9.0.0, and
`crates/dv-media/Cargo.toml` takes `ffmpeg-next` with `default-features = false`
and `codec`, `format`, `software-resampling`, `software-scaling`. So the link needs
libavcodec, libavformat, libavutil, libswresample and libswscale, and not
libavdevice or libavfilter. The headers must say libavcodec major 63. That is
ffmpeg-sys-next 9's newest version flag, `ffmpeg_9_0` = libavcodec 63.1, and it is
what Linux builds against today. Arch's `ffmpeg 2:9.0.1-4` has libavcodec and
libavformat 63.1.101, libavutil 61.1.101, libswresample 7.1.101 and libswscale
10.1.101.
`ffmpeg-sys-next` looks for FFmpeg in this order. First `FFMPEG_DIR`, taking
`include/` and `lib/` under it and linking `avcodec` and the rest by name, which
under MSVC means `avcodec.lib`. Then vcpkg, on MSVC targets only. Then pkg-config.
Its build script also compiles and runs a small C program against the headers, and
runs bindgen, which loads libclang.

Nothing links pdfium at build time. `pdfium-render` has no `static` feature and
opens the library at run time, so the CI jobs fetch none. B6.8 and B6.12 are for
packaging.

### 2.1 Linux

System FFmpeg 9 via pacman. pdfium optional at runtime as today. Nothing changes.

### 2.2 macOS

- [x] **B6.7** Establish how to get FFmpeg with **libavcodec major 63** on the macOS
      runner. Try in order and record the winner in the Decisions log:
      1. `brew install ffmpeg` then `pkg-config --modversion libavcodec`; use it if the
         major is 63.
      2. A versioned formula (`ffmpeg@9`) if Homebrew has one by then.
      3. Build FFmpeg from source into `$HOME/ffmpeg-prefix` with
         `--enable-shared --disable-static --disable-programs --disable-doc
         --enable-gpl` and the decoders the app uses, cached by the FFmpeg version
         string; export `PKG_CONFIG_PATH=$HOME/ffmpeg-prefix/lib/pkgconfig`.
      Done when: `cargo build -p dv-media` succeeds on the runner and the log shows
      which option was used. — written 2026-09-29, unverified until the first push.
      The workflow uses option 1 on the evidence in the Decisions log; its FFmpeg
      step prints `FFmpeg: B6.7 option 1, Homebrew ffmpeg <version>, libavcodec
      <version>` and fails, pointing here, when the major is not 63. Switching to
      option 2 or 3 is below. — done 9afe895, verified on the runner (2026-09-30
      note): option 1; run 36669749789 logs `FFmpeg: B6.7 option 1, Homebrew ffmpeg
      9.0.1_1, libavcodec 63.1.101` and builds dv-media.
- [x] **B6.8** pdfium for macOS: download `pdfium-mac-arm64.tgz` from the
      `bblanchon/pdfium-binaries` release matching the `pdfium_7881` ABI feature in
      `crates/df-app/Cargo.toml` (chromium/7881 or the nearest release that keeps the
      ABI; the pdfium-render docs list the pairing). Store the URL and sha256 in
      `build/macos/pdfium.lock` (a two-line text file). The runner fetches and verifies
      it. Done when: the file exists and the fetch step verifies the hash.
      Researched 2026-09-29: the release exists, and its URL and sha256 are in the
      Decisions log. — done 19de00b, verified on the runner:
      `build/macos/fetch-pdfium.sh` reads the lock, keeps the tarball in
      `target/pdfium/mac-arm64/` (the release workflow caches it by the lock's
      hash), checks the sha256 on every run and unpacks `libpdfium.dylib` and
      pdfium's licenses. The release run's macos job logs `pdfium:
      chromium/7881/pdfium-mac-arm64.tgz, sha256 52e94ca5…, verified` (run
      36729167091, dry run).
- [x] **B6.9** Confirm `libclang` is available for `bindgen` on the runner (Xcode's
      is). Done when: `dv-media` builds without a `LIBCLANG_PATH` override, or the
      override is set in the workflow with a comment. — written 2026-09-29,
      unverified until the first push. The workflow sets no override. The Runner step
      logs `xcode-select -p`, the Xcode whose toolchain clang-sys searches for
      libclang. If bindgen cannot find libclang, add to the FFmpeg step:
      `echo "LIBCLANG_PATH=$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib" >> "$GITHUB_ENV"`.
      — done 9afe895, verified on the runner (2026-09-30 note): no override;
      dv-media builds in run 36669749789 against Xcode 26.6's libclang
      (`/Applications/Xcode_26.6.app/Contents/Developer`).

**Switching B6.7 to a fallback** (researched 2026-09-29). Each replaces the `brew
install` line of the `FFmpeg (Homebrew)` step in `.github/workflows/ci.yml`. The
libavcodec-63 check after it stays; change the `option 1` in its echo.

- *Option 2, `ffmpeg@9`.* Homebrew has no `ffmpeg@9` today, because `ffmpeg` itself
  is 9.x. It keeps the previous major as a versioned formula when a new one lands,
  and `ffmpeg@8`, `@7`, `@6`, `@5` and `@4` exist now, so expect `ffmpeg@9` when
  `ffmpeg` moves to 10. It is keg-only, so pkg-config has to be told where it is, in
  this step for the check and through `GITHUB_ENV` for the cargo steps.
  ```bash
  brew install ffmpeg@9 pkgconf
  export PKG_CONFIG_PATH="$(brew --prefix ffmpeg@9)/lib/pkgconfig"
  echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH" >> "$GITHUB_ENV"
  ```
- *Option 3, from source*, cached by version. Add a cache step in front of the
  FFmpeg step, with the same `actions/cache` pin as the cargo cache.
  ```yaml
  - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
    with:
      path: ~/ffmpeg-prefix
      key: ffmpeg-9.0.2-${{ runner.os }}-${{ runner.arch }}
  ```
  Then replace the `brew install` line with this.
  ```bash
  prefix="$HOME/ffmpeg-prefix"
  if [ ! -f "$prefix/lib/pkgconfig/libavcodec.pc" ]; then
    curl -fsSL https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz | tar xJ
    (cd ffmpeg-9.0.2 &&
      ./configure --prefix="$prefix" --enable-shared --disable-static \
        --disable-programs --disable-doc --enable-gpl \
        --disable-avdevice --disable-avfilter &&
      make -j"$(sysctl -n hw.ncpu)" install)
  fi
  export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
  echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH" >> "$GITHUB_ENV"
  ```
  Keep the version in the cache key and the URL the same. avdevice and avfilter are
  off because nothing links them, as the §2 intro says. Without external libraries this build
  has no software AV1 decoder, so AVIF previews would not decode. That does not
  matter for CI. For a release build add `--enable-libdav1d` (after
  `brew install dav1d`) and bundle `libdav1d` with the rest (B6.16). The dylibs'
  install names are absolute paths under `$prefix`, so the tests and the smoke test
  find them on the runner.

### 2.3 Windows

- [x] **B6.10** FFmpeg for Windows: download a `ffmpeg-n9.*-win64-gpl-shared` build
      from `BtbN/FFmpeg-Builds` releases (GPL matches the project licence). Pin the
      exact asset name and sha256 in `build/windows/ffmpeg.lock`. The workflow
      extracts it and sets `FFMPEG_DIR` to the extracted directory, which is what
      `ffmpeg-sys-next` reads on Windows. Done when: `cargo build -p dv-media` passes
      on the runner. — written 2026-09-29, unverified until the first push. The pin
      (`FFMPEG_URL`, `FFMPEG_SHA256`) is in the `windows` job's `env:` in
      `ci.yml` for now: `build/` was outside that change, so
      `build/windows/ffmpeg.lock` does not exist yet. The step checks the sha256,
      extracts with `7z`, fails unless `version_major.h` says 63, and sets
      `FFMPEG_DIR`. — done b96afb7, verified on the runner: the pin is
      `build/windows/ffmpeg.lock` now, and `build/windows/fetch-ffmpeg.ps1` does
      the download, the sha256 check (every run), the unpacking (Windows' own
      `tar.exe`) and the major-63 check for both workflows. dv-media builds with it
      in CI run 36729161540 and the release run 36729167091.
- [x] **B6.11** `bindgen` on Windows needs `LIBCLANG_PATH`. The `windows-latest`
      image has LLVM under `C:\Program Files\LLVM`; set the variable in the workflow.
      Done when: build passes. — written 2026-09-29, unverified until the first
      push. The image's LLVM is 20.1.8, installed by Chocolatey into that directory.
      The step falls back to Visual Studio's bundled LLVM (`VC\Tools\LLVM\x64\bin`
      via `vswhere`), which is what rust-ffmpeg's own Windows CI uses, when
      `libclang.dll` is not there. — done 9afe895, verified on the runner
      (2026-09-30 note): run 36669749789 logs `LIBCLANG_PATH=C:\Program
      Files\LLVM\bin` and builds.
- [x] **B6.12** pdfium for Windows: `pdfium-win-x64.tgz` from the same
      `bblanchon/pdfium-binaries` release as B6.8, pinned in
      `build/windows/pdfium.lock`. Done when: fetched and verified in the job.
      Researched 2026-09-29: the release exists, and its URL and sha256 are in the
      Decisions log. — done 3585685, verified on the runner:
      `build/windows/fetch-pdfium.ps1` reads the lock, keeps the tarball in
      `target\pdfium\win-x64\`, checks the sha256 on every run and unpacks
      `pdfium.dll` and pdfium's licenses; the release run's windows job logs
      `pdfium: chromium/7881/pdfium-win-x64.tgz, sha256 73cc0de6…, verified` (run
      36729167091).
- [x] **B6.13** The `windows` job's smoke test must run with the FFmpeg `bin/`
      directory on `PATH` so the DLLs resolve. Done when: `delightfile --version`
      prints on the runner. — written 2026-09-29, unverified until the first push.
      The FFmpeg step appends `bin\` to `GITHUB_PATH`, so the df-app tests get it
      too. — done 9afe895, verified on the runner (2026-09-30 note): the smoke test
      checks the printed `delightfile 0.1.0` since W4.1 (run 36662477949), and is
      green on `main` in run 36669749789.

**Switching B6.10 to another build** (researched 2026-09-29). The step derives the
directory name from the URL (the zip's top directory is the asset name without
`.zip`, for BtbN's builds and gyan.dev's alike), so a switch changes only
`FFMPEG_URL` and `FFMPEG_SHA256`, which since 2026-09-30 are the two lines of
`build/windows/ffmpeg.lock`:

- *A newer BtbN build.* BtbN keeps the last build of each month for two years and
  the last 14 daily builds (its README, "Release Retention Policy"). The `latest`
  release is rebuilt daily, so its hash changes every day and it cannot be pinned.
  Take a month-end `autobuild-YYYY-MM-DD-HH-MM` release and its
  `ffmpeg-n9.<x>-…-win64-gpl-shared-9.<y>.zip`. The sha256 is that asset's line in
  the release's `checksums.sha256` (the API's `digest` field agrees).
- *gyan.dev's release build*, which is what rust-ffmpeg's own Windows CI links
  against, with `FFMPEG_DIR` and `LIBCLANG_PATH` set as here. The versioned GitHub
  mirror is `GyanD/codexffmpeg`. The 9.0.1 zip, downloaded and listed on
  2026-09-29, has `lib/avcodec.lib`, `avformat.lib`, `avutil.lib`,
  `swresample.lib`, `swscale.lib` and `bin/avcodec-63.dll`. Its two lines are
  these.
  ```yaml
  FFMPEG_URL: https://github.com/GyanD/codexffmpeg/releases/download/9.0.1/ffmpeg-9.0.1-full_build-shared.zip
  FFMPEG_SHA256: 6fd54b3b4f49117a307877b570f5e1659090f178973298658b41f5c559b5b5ab
  ```
  9.0.2 is `…/9.0.2/ffmpeg-9.0.2-full_build-shared.zip`, sha256
  `8d31e162f1616e37aab3fa2db991b97e1b8dbeb1c8465fd81a81be16ff91b328`. That hash is
  the GitHub API's; the file was not downloaded.
- *vcpkg*, last, because it builds FFmpeg from source (tens of minutes; cache
  `C:\vcpkg\installed`). The image has vcpkg at `C:\vcpkg` (`VCPKG_INSTALLATION_ROOT`),
  and vcpkg's `ffmpeg` port is 9.0.2 on its master branch today. The image's checkout
  is pinned to an older commit. Replace the FFmpeg step's body with
  `git -C C:\vcpkg pull`, then
  `vcpkg install ffmpeg[core,avcodec,avformat,swresample,swscale]:x64-windows`,
  then add `VCPKG_ROOT=$env:VCPKG_INSTALLATION_ROOT` to `GITHUB_ENV` and
  `C:\vcpkg\installed\x64-windows\bin` to `GITHUB_PATH` (for B6.13). Drop the
  `FFMPEG_*` env lines. With `FFMPEG_DIR` unset, ffmpeg-sys-next asks the vcpkg
  crate, which reads `VCPKG_ROOT`.

## 3. Packaging

Scripts live under `build/<os>/` and are runnable by hand on that OS, not only from
the workflow, so a human with the machine can produce the same artifact.

### 3.1 Linux (`build/linux/`)

- [x] **B6.14** `build/linux/package.sh` produces
      `delightfile-<version>-x86_64-linux.tar.gz` containing `delightfile`,
      `build/install.sh`, the wrapper script, the desktop files and icons that
      `install.sh` installs (see appendix B §5 for the list — including
      `build/delightfile.portal` and
      `build/org.freedesktop.impl.portal.desktop.delightfile.service.in`, which
      `install_portal` needs). `install.sh` changes so it works from the tarball:
      `bin` (`install.sh:23–26`) is discovered as `./delightfile` next to the script
      when `target/release/delightfile` is absent; `reload_bus` (`:172–177`,
      `busctl --user`) and the `sudo` path in `as_root` (`:65–75`) are guarded so a
      missing `busctl`/session bus prints a note instead of aborting under `set -e`.
      Done when: extracting the tarball on a clean Arch container (no session bus)
      and running `bash install.sh` installs a working binary and prints the
      portal-restart note.
      — done 0a8d648, verified on the runner: `build/linux/check-package.sh`
      unpacks the tarball, installs it into an empty home as an unprivileged user
      with nobody at a terminal, runs the installed `--version`, looks for every
      installed file and the restart note, and uninstalls. The release run's
      `verify-linux` job passes it in a fresh `archlinux:latest` with only
      `ffmpeg` added (run 36729167091), and so does a local container on the
      tarball downloaded from that run. `install.sh` from a checkout is unchanged
      (Decisions log, 2026-09-30).

### 3.2 macOS (`build/macos/`)

- [x] **B6.15** `build/macos/Info.plist` with `CFBundleIdentifier`
      `com.showandtour.delightfile` (Brian's domain; change only if he says so),
      `CFBundleExecutable delightfile`, `CFBundleName delightfile`,
      `CFBundleIconFile delightfile.icns`, `CFBundleShortVersionString` and
      `CFBundleVersion` filled from `Cargo.toml` at package time, `LSMinimumSystemVersion`
      `13.0`, `NSHighResolutionCapable true`, `CFBundleDocumentTypes` declaring
      `public.folder` so Finder offers the app for folders, and
      `NSSupportsAutomaticGraphicsSwitching true`. Done when: `plutil -lint` passes.
      — done db8fdae, verified on the runner: `plutil -lint` passes on the filled
      plist in the release run's macos job and again on the app in the dmg
      (run 36729167091). `LSMinimumSystemVersion` is not `13.0` but the bundle's
      real minimum, 26.0 at first and 15.0 since the release builds on
      `macos-15` (6456c15, dry run 36734749641); and since e420ed4 there is
      no `CFBundleDocumentTypes` until M2.36 in `02-macos.md` (Decisions log,
      2026-09-30).
- [x] **B6.16** `build/macos/bundle.sh`: assembles `delightfile.app` from
      `target/release/delightfile`: `Contents/MacOS/delightfile`, `Contents/Info.plist`,
      `Contents/Resources/delightfile.icns` (generated from the PNG icons in `build/`
      with `iconutil`), `Contents/Frameworks/` holding `libpdfium.dylib` and every
      `libav*`/`libsw*` dylib the binary links (discover with `otool -L`, copy
      transitively), then `install_name_tool -change <old> @executable_path/../Frameworks/<name>`
      for each and `-id` on each copied dylib. Ad-hoc sign the result
      (`codesign --force --deep --sign -`) so the app runs at all on Apple Silicon,
      which refuses unsigned arm64 binaries outright. Done when: on the runner,
      `open`-less validation passes: `codesign --verify --deep --strict`, `otool -L`
      shows no `/opt/homebrew` or `$HOME` paths, and `Contents/MacOS/delightfile
      --version` runs.
      — done 7a49209, verified on the runner: the release run's macos job
      (36729167091) bundles the program, pdfium and 16 Homebrew libraries
      (libav*, libsw*, libvpx, liblzma, libdav1d, libmp3lame, libmpg123,
      libopus, libSvtAv1Enc, libx264, libx265, libssl, libcrypto), 41 MB of
      Frameworks. `build/macos/check-app.sh` passes: `codesign --verify --deep
      --strict`, `otool -L` on the program and every library naming only
      `/usr/lib`, `/System` and `@executable_path/../Frameworks/…`, and
      `--version` printing `delightfile 0.1.0` with every image dyld loads
      (`DYLD_PRINT_LIBRARIES`) from the system or the bundle. The icon is drawn
      from the SVG, not the PNGs (Decisions log, 2026-09-30).
- [x] **B6.17** `build/macos/dmg.sh`: `hdiutil create` a compressed dmg
      `delightfile-<version>-aarch64-macos.dmg` containing the `.app` and an
      `Applications` symlink. Done when: the dmg mounts and the app inside passes
      B6.16's checks.
      — done f19f9f9, verified on the runner: `build/macos/check-dmg.sh`
      verifies the image, mounts it read-only, finds the Applications link, runs
      `check-app.sh` on the app where it stands and unmounts it; it passes in the
      macos job and again in `verify-macos`, a fresh runner without Homebrew's
      FFmpeg (run 36729167091). The dmg is 28 MB, HFS+ UDZO.
- [x] **B6.18** The app must find `libpdfium.dylib` in `Contents/Frameworks` at
      runtime: Phase 1/2 adds that search path in the pdfium loader
      (`02-macos.md` cross-references). Done when: PDF preview works in the bundled
      app on a real Mac (`07-verification.md` V7.x) — until then, done at "the loader
      logs the path it tried".
      — done df33c49, at "the loader logs the path it tried":
      `platform/macos/pdfium.rs` puts `<exe>/../Frameworks/libpdfium.dylib` after
      `DF_PDFIUM_LIB`, and `preview::doc::pdf` logs each candidate it tries at
      debug. The bundle puts the library there (B6.16). PDF preview in the bundle
      on a Mac is still the live check.

### 3.3 Windows (`build/windows/`)

- [x] **B6.19** `build/windows/package.ps1` produces
      `delightfile-<version>-x86_64-windows.zip` containing `delightfile.exe`, the
      FFmpeg `bin/*.dll` set from B6.10 (only the libraries the exe imports: check with
      `dumpbin /dependents`, and their transitive deps), `pdfium.dll`, and a
      `README.txt` with the SmartScreen note from §6. Done when: extracting the zip
      on a clean `windows-latest` runner and running `delightfile.exe --version`
      works without any other install.
      — done b20848f, verified on the runner: the zip holds `delightfile.exe`,
      avcodec-63, avformat-63, avutil-61, swresample-7, swscale-10,
      `pdfium.dll`, `README.txt`, `LICENSE.txt` and `licenses\` (63 MB), and
      `package.ps1` refuses a folder where anything imports a DLL that is neither
      there nor Windows' own. `build/windows/check-package.ps1` unpacks it into an
      empty folder and runs `--version` with no FFmpeg folder on `PATH`: it
      prints `delightfile 0.1.0` in the windows job and in `verify-windows`, a
      fresh runner that never fetched FFmpeg (run 36729167091). The only DLLs
      outside Windows the exe and its DLLs import are the bundled ones (the C
      runtime is static, FFmpeg's DLLs use the UCRT that Windows 10 and 11 carry,
      pdfium imports only kernel32, user32, gdi32 and advapi32).
- [x] **B6.20** Icon and version resource: `build/windows/delightfile.rc` referencing
      `delightfile.ico` (generated from the PNG icons with ImageMagick or a small
      script in `build/windows/`) and a `VERSIONINFO` block. `crates/df-app/build.rs`
      compiles it with `rc.exe` (always present with the MSVC toolchain) only when
      `CARGO_CFG_WINDOWS` is set, and emits `cargo:rustc-link-arg=<path>.res`. No
      `winres`/`embed-resource` crate. Done when: Explorer shows the icon on the exe
      and `--version`'s number matches the file properties.
      — done 83c18a4, verified on the runner: `check-package.ps1` reads the
      unpacked exe's version resource (FileVersion and ProductVersion `0.1.0`,
      what `--version` prints) and finds one icon group in it
      (`ExtractIconExW`), in the windows job and in `verify-windows` (run
      36729167091); locally `llvm-readobj --coff-resources` on the downloaded exe
      shows the GROUP_ICON, nine ICON sizes and VERSIONINFO. The icon is drawn
      from the SVG by ImageMagick on the runner, not committed (Decisions log,
      2026-09-30). How Explorer draws it is for a person at Windows.
- [x] **B6.21** `#![windows_subsystem = "windows"]` in `main.rs` gated on
      `cfg(windows)` so no console opens, and `--version`/`--help` still print by
      attaching to the parent console (`AttachConsole(ATTACH_PARENT_PROCESS)` via
      `windows-sys`, attempted unconditionally at startup — it fails harmlessly when
      the app was launched from Explorer). This is a Phase 4 code
      task (`04-windows.md` W4.1); it is listed here because the packaging smoke test
      depends on it. Done when: B6.19's smoke test prints the version in the runner log.
      — done 5722b0a (W4.1), verified on the runner: B6.19's check prints
      `--version: delightfile 0.1.0` from the unpacked zip (run 36729167091).

## 4. Release workflow

- [>] **B6.22** `.github/workflows/release.yml` on tags `v*`: a `check-version` job
      that fails unless the tag equals `v` + the workspace version; then the three
      build jobs reusing the CI steps and the §3 scripts; then a `publish` job that
      creates a GitHub Release with the three artifacts and their `.sha256` files,
      marked pre-release until Brian edits it. Done when: a dry-run on a `v0.0.0-test`
      tag on a branch produces all three artifacts (delete the tag afterwards).
      — written 941060b, needs the first tag. No tag was pushed (Brian's rule for
      this work); the dry run is a dispatch instead, with `dry_run` on, and run
      36730956539 on `port/release` (5edac47) produced all three artifacts and
      checked each on a fresh runner (below). The `version` job is the
      check-version job; `publish` makes a draft pre-release with
      `gh release create --verify-tag` and has not run.
- [>] **B6.23** Release notes: the workflow drafts them from the commit subjects
      since the previous tag. Brian edits before publishing. Done when: the draft
      appears.
      — written 941060b, needs the first tag: the dry run's `notes.md` holds the
      install lines and the subjects, cut at 120,000 characters (462 commits and
      no previous tag: 274 listed, 188 left out and counted). It becomes the
      draft's text on the first tag.

**Dry run, 2026-09-30.** `gh workflow run release.yml --ref port/release -f
dry_run=true`, run 36730956539 at 5edac47, green, 13 min 19 s. What it uploaded as
the `release` artifact, downloaded and checked here with `sha256sum -c`:

| File | Bytes | What was checked, and where |
|---|---:|---|
| `delightfile-0.1.0-x86_64-linux.tar.gz` | 11,682,933 | `verify-linux`: fresh `archlinux:latest` with only `ffmpeg`, `check-package.sh` as an unprivileged user. Again in a local container. |
| `delightfile-0.1.0-aarch64-macos.dmg` | 28,445,723 | macos job and `verify-macos` (no Homebrew FFmpeg): `hdiutil verify`, mounted read-only, `check-app.sh` on the app there (codesign, `otool -L`, `--version`, dyld's loads), unmounted. Unpacked here with 7-Zip: `LSMinimumSystemVersion` 26.0, 17 dylibs in 41 MB of Frameworks, a 1024 px transparent icon. |
| `delightfile-0.1.0-x86_64-windows.zip` | 63,163,379 | windows job and `verify-windows` (never had FFmpeg): unpacked, `--version` with no FFmpeg on `PATH`, version resource `0.1.0`, one icon group. |
| `delightfile.rb` | 1,596 | `brew audit --cask --strict` and `brew style` clean in a local tap on Homebrew 6.0.22; installed from the dmg through the tap, quarantined (`com.apple.quarantine … Homebrew Cask`), the caveats' `xattr` line, then `check-app.sh` on `/Applications/delightfile.app`, then uninstalled. |
| `delightfile.json` | 1,436 | Scoop 0.6.0 from its installer at `1e2f334`, the manifest served from the runner: installed, shim and Start menu shortcut made, the installed exe's `--version` right, uninstalled. |
| `notes.md` | 120,227 | Drafted, cut to fit (B6.23). |
| three `.sha256` | ~100 each | `sha256sum -c` here and in each verify job. |

**Dry run on `port/release-2`, 2026-09-30**, after the decisions on the macOS
minimum, the document types, the licenses and scoop's notes: run 36736333110 at
e9b1c8c, green, with `verify-macos` on macOS 15.7.9 (`macos-15`) and 26.6.2
(`macos-latest`). Downloaded and checked here as before: the tarball
(11,683,227 bytes) installs in a clean local Arch container; the dmg
(28,931,060 bytes) unpacks to an app with `LSMinimumSystemVersion` 15.0, no
`CFBundleDocumentTypes`, 17 dylibs linked only through
`@executable_path/../Frameworks` and the system, and
`Contents/Resources/Licenses` holding `delightfile/LICENSE`,
`FFmpeg/COPYING.GPLv3`, `FFmpeg/LICENSE.md`, `pdfium/LICENSE` with the 15
licenses of what pdfium builds in, and `SOURCES.txt`; the zip (63,164,215 bytes)
holds `Licenses\delightfile\LICENSE.txt`, `Licenses\FFmpeg\COPYING.GPLv3.txt`,
`Licenses\pdfium\LICENSE.txt` with the same 15 and `Licenses\SOURCES.txt`; the
cask says `depends_on macos: :sequoia`; scoop prints the manifest's notes with
`$dir` filled in. The run before it, 36734749641 at ac5257a, was green too; e9b1c8c
added x264's commit to the macOS `SOURCES.txt` and a final newline to the
Windows one.

**Dry run on `port/windows-tidy`, 2026-09-30**, after FFmpeg's `LICENSE.md`
joined the Windows zip (Decisions log): run 36743611087 at de9028c, green, with
`verify-macos` on both runners and `publish` skipped. The zip (63,205,238
bytes, `sha256sum -c` clean here) holds `Licenses\SOURCES.txt`,
`Licenses\delightfile\LICENSE.txt`, `Licenses\FFmpeg\COPYING.GPLv3.txt`,
`Licenses\FFmpeg\LICENSE.md.txt` and `Licenses\pdfium\LICENSE.txt` with the
same 15. `LICENSE.md.txt` is 4,346 bytes with sha256 `2e1d16c7…`, the committed
`build/windows/FFmpeg-LICENSE.md` byte for byte. `verify-windows` found each
file and printed `SOURCES.txt`, whose FFmpeg entry names `LICENSE.md.txt` as
FFmpeg's `LICENSE.md` at `e47273f4d9`.

## 5. Distribution channels

- [>] **B6.24** Homebrew tap: a repository `brianschwabauer/homebrew-tap` with
      `Casks/delightfile.rb` pointing at the dmg URL and sha256, `depends_on macos:`
      the bundle's own minimum (`:sequoia` since the release builds on `macos-15`;
      B6.24 said `">= :ventura"`, see the
      Decisions log, 2026-09-30), and a `caveats` block with the unsigned-app
      instructions (§6): Homebrew **does** quarantine cask downloads, and Homebrew 6
      removed `--no-quarantine` (and `HOMEBREW_CASK_OPTS=--no-quarantine`), which
      this task first recommended, so the caveats give
      `xattr -dr com.apple.quarantine /Applications/delightfile.app`.
      The release workflow opens a PR against the tap bumping version and sha256
      (needs a `TAP_TOKEN` secret with contents write on that repo). Brian creates the
      repository and the token; the agent writes the cask and the workflow step.
      Done when: `brew install --cask brianschwabauer/tap/delightfile` installs on a
      Mac (`07-verification.md`), or until then, `brew audit --cask` passes on the
      runner.
      — written ea80186 and 42ecb05, needs the first tag and the tap. The cask is
      `build/homebrew/delightfile.rb`; the release workflow fills in the version,
      the dmg's sha256 and `depends_on macos:` and attaches it as `delightfile.rb`.
      On the dry run's `verify-macos` (36730956539) it passes `brew audit --cask
      --strict` and `brew style` and installs from the dmg; the first dry run's
      `brew style` wanted `:tahoe` rather than `">= :tahoe"` (42ecb05). The tap
      repository does not exist, and the pull-request step is not written: both
      wait for Brian (Decisions log, 2026-09-30).
- [>] **B6.25** scoop bucket: repository `brianschwabauer/scoop-bucket` with
      `bucket/delightfile.json` (`url`, `hash`, `bin`, `checkver` on GitHub releases,
      `autoupdate`). Release workflow bumps it the same way. Done when: `scoop bucket
      add` + `scoop install delightfile` works on the runner.
      — written 6cf9307 and 5edac47, needs the first tag and the bucket. The
      manifest is `build/scoop/delightfile.json`, with a Start menu shortcut beside
      `bin`, and `autoupdate` takes the hash from the zip's `.sha256`; the workflow
      fills it in and attaches it as `delightfile.json`. On the dry run's
      `verify-windows` (36730956539), `scoop install` of the filled manifest works
      and the installed program's `--version` is right. `scoop bucket add` waits
      for the bucket repository; the shim is a GUI one, which the manifest's
      `notes` say since ac5257a (Decisions log, 2026-09-30).
- [>] **B6.26** README: an "Installing" section per platform with the exact commands
      and the unsigned-app notes. Done when: reviewed by Brian.
      — written ab1faba, marked as pending the first release; waits for Brian's
      review.
- [~] **B6.27** winget manifest — skipped until there is a signed build or a user asks;
      winget's review process is slow and a `[~]` here keeps the option visible.

## 6. The signing decision

What "unsigned" costs users:

| Platform | First launch experience | Workaround the README documents |
|----------|------------------------|--------------------------------|
| macOS 14 | Gatekeeper: "cannot be opened because the developer cannot be verified"; right-click → Open works | Right-click, Open, Open |
| macOS 15+ | Same dialog but right-click → Open no longer bypasses it | System Settings → Privacy & Security → scroll to "delightfile was blocked" → Open Anyway; or `xattr -d com.apple.quarantine /Applications/delightfile.app` |
| Homebrew cask | Homebrew quarantines cask downloads, so `brew install --cask` hits the same dialog as the dmg; Homebrew 6 has no `--no-quarantine` (Decisions log, 2026-09-30) | `xattr -dr com.apple.quarantine /Applications/delightfile.app` after the install; the caveats say so |
| Windows | SmartScreen "Windows protected your PC" on the downloaded zip's exe | More info → Run anyway; scoop installs are not SmartScreen-checked |

Why not sign now:

- macOS signing needs an Apple Developer Program membership (99 USD/year) and a
  Developer ID certificate. It does **not** need a Mac: the CSR can be generated with
  openssl on Linux, the certificate is issued in the developer portal, and
  `codesign`/`notarytool`/`stapler` all run on the GitHub macOS runner. So the lack of
  a Mac is not the blocker; the yearly fee for a hobby release is, and the fact that
  nobody has run the app on a Mac yet makes a notarized first release premature.
- Windows Authenticode: OV certificates now require a hardware token or a cloud HSM,
  which CI cannot use directly. Azure Trusted Signing is cheap but has identity
  validation requirements that change. SignPath.io offers free signing for open
  source projects and integrates with GitHub Actions; that is the first thing to try
  if Windows signing is ever wanted.

## 7. Optional: notarized macOS release (only if Brian reverses §6)

- [~] **B6.28** Certificate: on Linux, `openssl req -new -newkey rsa:2048 -nodes
      -keyout dev.key -out dev.csr`, upload the CSR at developer.apple.com →
      Certificates → Developer ID Application, download the `.cer`, then
      `openssl pkcs12 -export` with the key into `dev.p12`; store base64 of the `.p12`
      as `MAC_CERT_P12` and its password as `MAC_CERT_PASSWORD`. App Store Connect API
      key (Keys → Team key with Developer access): store `.p8` contents, key id and
      issuer id as secrets.
- [~] **B6.29** Workflow steps after B6.16: create a temporary keychain, import the
      p12, `codesign --force --options runtime --timestamp --entitlements
      build/macos/entitlements.plist --sign "Developer ID Application: …"` on every
      dylib in `Frameworks/` and then the app; `xcrun notarytool submit <dmg>
      --key … --key-id … --issuer … --wait`; `xcrun stapler staple <dmg>`.
- [~] **B6.30** `build/macos/entitlements.plist`: hardened runtime needs
      `com.apple.security.cs.disable-library-validation` **only if** pdfium is not
      signed with the same identity; since B6.29 signs every dylib, no entitlements
      beyond the defaults are expected. Verify with `spctl --assess --type execute`.

## Decisions log

- 2026-09-25 — Arch container for Linux CI (FFmpeg 9 availability, matches the
  development machine).
- 2026-09-25 — Not signing initially; Homebrew tap and scoop as the primary channels.
- 2026-09-25 — Bundle FFmpeg and pdfium on macOS and Windows; ad-hoc codesign the
  macOS bundle so it launches on Apple Silicon.
- 2026-09-25 — No resource-embedding crate on Windows; `build.rs` calls `rc.exe`.
- 2026-09-29 — FFmpeg requirement, read from the repository rather than this
  document: `ffmpeg-next`/`ffmpeg-sys-next` 9.0.0 (`Cargo.lock`) with `codec`,
  `format`, `software-resampling` and `software-scaling` (`crates/dv-media/Cargo.toml`).
  That means libavcodec, libavformat, libavutil, libswresample and libswscale at
  libavcodec major 63. Arch has `ffmpeg 2:9.0.1-4` (libavcodec 63.1.101). The macOS
  and Windows jobs fail with a pointer to §2 when their FFmpeg is not major 63. The
  Linux job only logs the version, because Arch is the reference and moves with
  Brian's machine.
- 2026-09-29 — macOS FFmpeg: B6.7 option 1, `brew install ffmpeg`. Evidence:
  `formulae.brew.sh/api/formula/ffmpeg.json` gives stable 9.0.2 with `arm64_tahoe`
  and `arm64_sequoia` bottles (none for `arm64_sonoma`) and versioned formulae
  `ffmpeg@8` … `@2.8`. `macos-latest` is macOS 26 arm64
  (actions/runner-images README; image 20260907.0351.1 has Homebrew 6.0.22,
  pkgconf 3.0.7 and Xcode's clang). The README also shows Intel macOS only under
  `-intel`/`-large` labels now (`macos-15-intel`, `macos-26-intel`) and `macos-14`
  deprecated; "Decisions already made" still holds for the plain labels.
  rust-ffmpeg's own CI (zmwangx/rust-ffmpeg `.github/workflows/build.yml`) builds
  with `brew install ffmpeg pkg-config` on `macos-latest`. Options 2 and 3 are
  written out in §2.2, "Switching B6.7 to a fallback". Unverified until the first
  push.
- 2026-09-29 — Windows FFmpeg: BtbN
  `autobuild-2026-08-31-13-27/ffmpeg-n9.0.1-11-ge47273f4d9-win64-gpl-shared-9.0.zip`,
  76,434,028 bytes, sha256
  `00d78694632f17a1de325c639d0acf04a6b3ab8f20ce0a2e5bedd2d5e21e3adb`. Downloaded
  and hashed locally; the hash matches the release's `checksums.sha256` and the API
  digest. It holds `include/` at libavcodec 63.1.101, `bin/avcodec-63.dll` …, and
  MSVC import libraries `lib/avcodec.lib`, `avformat.lib`, `avutil.lib`,
  `swresample.lib` and `swscale.lib`, which is what `FFMPEG_DIR` needs under
  `link.exe`. A month-end build because BtbN keeps those for two years; daily
  builds are kept 14 days, and `latest` changes daily and cannot be hash-pinned.
  The fallbacks, gyan.dev's 9.0.1 build (also checked locally) and vcpkg, are
  written out in §2.3, "Switching B6.10 to another build". Unverified until the
  first push.
- 2026-09-29 — pdfium: the CI jobs fetch none, because `pdfium-render` has no
  `static` feature and nothing links it at build time. For B6.8/B6.12:
  `bblanchon/pdfium-binaries` release `chromium/7881` exists (published
  2026-06-08; latest is `chromium/8076`, which is not the ABI `pdfium_7881` asks
  for). Both assets downloaded and hashed locally:
  `https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7881/pdfium-mac-arm64.tgz`
  sha256 `52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40`
  (holds `lib/libpdfium.dylib`), and `…/chromium/7881/pdfium-win-x64.tgz` sha256
  `73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08` (holds
  `bin/pdfium.dll`). The lock files belong under `build/`, which was outside this
  change.
- 2026-09-29 — Windows `LIBCLANG_PATH`: `C:\Program Files\LLVM\bin`. The
  windows-2025 image installs LLVM 20.1.8 there through Chocolatey
  (runner-images `Install-LLVM.ps1`). The step falls back to Visual Studio's
  bundled LLVM, found with `vswhere`, if `libclang.dll` is not there. macOS gets no
  override (B6.9).
- 2026-09-29 — Action pins are full commit SHAs with the tag in a comment:
  `actions/checkout` v7.0.1 `3d3c42e5aac5ba805825da76410c181273ba90b1`,
  `actions/cache` v6.1.0 `55cc8345863c7cc4c66a329aec7e433d2d1c52a9`, and
  `dtolnay/rust-toolchain` master `02cb101ec7c40f2c49e1d9714d64511d8e1b74de` with
  `toolchain: stable`. B6.3 says `@stable`, which is a branch that moves; the pin
  installs the same toolchain from an action that does not. No other third-party
  actions.
- 2026-09-29 — The cache key carries a hash of the compiler as well as the job name
  and `Cargo.lock`, where B6.2 names only the last two. On Linux it is `rustc -vV`
  hashed; on macOS and Windows it is `dtolnay/rust-toolchain`'s `cachekey` output.
  actions/cache saves only when the key missed. Without the compiler in the key, a
  rust upgrade would restore a `target/` that cargo discards, and that stale cache
  would come back on every run until `Cargo.lock` changed. Arch's `rust` rolls and
  stable moves every six weeks, so this would happen often. `restore-keys` falls
  back only within the same compiler.
- 2026-09-29 — Three workflow-wide settings B6.1 does not name.
  `CARGO_INCREMENTAL=0`, because incremental artifacts do nothing in CI and make
  `target/`, and so the cache, bigger. `permissions: contents: read`, since the
  workflow only reads. `timeout-minutes: 90` on every job, so that a hung test ends
  the run instead of holding it for GitHub's six-hour default.
- 2026-09-29 — The macOS and Windows jobs run `cargo build -p df-core`,
  `cargo test -p df-core` and `cargo build -p dv-media` before the B6.1 steps. While
  those jobs are allowed to fail, the first failing step then shows how far each
  target gets. Phase 1 still exits only when every step is green.
- 2026-09-29 — Windows: `git config --global core.autocrlf false` before checkout,
  so that tests reading files from the tree see the same bytes as on Linux. The
  toolchain is named `stable-x86_64-pc-windows-msvc` rather than relying on the
  image's default host.
- 2026-09-29 — The Windows FFmpeg pin (`FFMPEG_URL`, `FFMPEG_SHA256`) lives in the
  job's `env:` in `ci.yml` until B6.10 creates `build/windows/ffmpeg.lock`.
- 2026-09-29 — The linux job builds and tests as an unprivileged user. The archlinux
  container runs every step as root, and permission bits do not stop root. Under
  `unshare -r` on the host, 13 tests that chmod a path to `0o000` or `0o555` and
  expect the I/O to fail saw it succeed and failed. In df-core they are
  `ops::copy::tests::a_move_that_cannot_happen_leaves_the_destination_alone`, five
  `ops::journal::tests::a_partial…`/`a_partly_undone…` tests, three `sync::tests`
  and `sync::rsync::tests::a_file_rsync_could_not_send_is_one_problem_not_two`,
  which the container skips for want of rsync. In df-app they are two
  `app::syncing::tests` and
  `app::tests::undo::a_copy_redo_that_cannot_land_goes_back_and_undo_works_again`.
  The choice was between running cargo as another user and making those tests skip
  under root, as `du/tests.rs:208`, `archive/write/tests.rs:727` and
  `vfs/rclone_tests.rs:1146` do. Running as another user won, because those tests
  must keep asserting something in CI, and nothing under `crates/` changes for CI's
  sake. After pacman the job runs `useradd --create-home --uid 1000 --user-group
  builder` and `git config --system --add safe.directory "$GITHUB_WORKSPACE"`, so
  git accepts the checkout whichever user owns it.
  `CARGO_HOME` is fixed at `/home/builder/.cargo` in the job's `env:`, and the cache
  paths name it. Checkout and the cache restore stay root. A step after the restore
  runs `chown -R builder:builder "$GITHUB_WORKSPACE" /home/builder`, and every
  cargo step and the smoke test run as `runuser -u builder -- …`. The uid is fixed
  so that a restored cache comes back owned by the user it was saved from; the chown
  covers the case where it does not. macOS runs as an ordinary user, and on
  Windows these tests do not exist, because they set Unix mode bits. In the local
  container run under §1 the 12 tests pass as builder, and a `target/` and cargo
  home left owned by root are handed over and build without a rebuild.
- 2026-09-29 — The macos and windows jobs run clippy with `-A dead_code` beside
  `-A clippy::chunks_exact_to_as_chunks`; the linux job does not (S1.53 in
  `01-platform-seam.md`). Off Linux, df-app has 49 dead-code warnings per
  target — the drag-and-drop helpers, gio's output readers, enum variants only
  the Wayland device constructs, `platform/icon.rs` — because their only
  callers are Linux bodies, found by building df-app on Linux with each
  target's platform modules selected. The rule for them is no `allow`
  attribute in the code and no `cfg` outside `platform/`, so the allowance is
  the workflow's. It comes off per job when Phase 2 (M2.31) and Phase 4
  (W4.34) give those items callers.
- 2026-09-29 — Of the four container failures in Open questions, the two that
  are not about fonts are fixed under `crates/` (S1.54 in
  `01-platform-seam.md`): `the_menus_pin_and_go` finds its row by name instead
  of by scan order, and an archive's `./` member is no longer unsafe. The two
  Nerd Font tests remain as that question describes.
- 2026-09-29 — The two Nerd Font failures from Open questions are fixed under
  `crates/`, and Brian approved the approach: a chrome glyph that no loaded
  face can draw is drawn by the app as a vector shape
  (`crates/df-app/src/glyphs.rs`). The failures were
  `icons::glyph_tests::the_chrome_glyphs_all_render` ("no face draws '✓'
  (U+2713)") and `menu::tests::a_key_stands_clear_of_the_chevron` ("the key is
  -0.84375 pt from the ▸"): without a Nerd Font, which includes every stock Mac
  and Windows install, the chrome drew ✓ and ▸ as missing-glyph boxes and
  measured them at the box's width. egui's stock faces lack ten of the chrome's
  symbols: ✓ ▸ ◂ ← ↑ → ↓ ⇧ ≡ ⟷ (Hack, the monospace face, lacks only ✓). Each
  shape takes the advance, baseline and colour of the text it sits in, centres
  on the stock face's math axis at that face's stroke weight, and scales with
  the font size and the display. Where any face has the glyph, as on Brian's
  machine with a Nerd Font, the text goes to egui unchanged, so nothing there
  moves. The other way the question offered, a Nerd Font such as
  `ttf-jetbrains-mono-nerd` on the container's pacman line, was not taken: CI
  would stop exercising the fontless path, and that path is what a stock Mac or
  Windows install gets. File-type icons are unchanged; without a Nerd Font they
  are still `ls -F` classifiers. df-app's tests pass with the font, with
  `/usr/share/fonts/TTF` and `~/.local/share/fonts` hidden in a mount namespace
  on the host, and in `archlinux:latest` with the linux job's pacman line and
  builder user, where the 5 `preview::doc::font` tests still skip for want of any
  system font.

- 2026-09-29 — The C runtime is linked statically on Windows (`.cargo/config.toml`,
  `+crt-static` for the msvc targets). The first CI build, started on a clean
  Windows 11 VM, stopped at "VCRUNTIME140.dll was not found" before `main`. The
  alternative, shipping or requiring the Visual C++ Redistributable, puts an
  installer in front of a program that is otherwise a folder to unzip. The same
  start opened a console window beside the program, which is W4.1/B6.21's to close.
- 2026-09-29 — The linux job's portal failure was in the test, not the program.
  `the_bus_starts_the_backend_on_the_first_call` in `crates/df-app/tests/portal.rs`
  kills its private `dbus-daemon` and waits for `/proc/<pid>` of the service that
  bus started to go away. The service stops at once. In run 36637010603 it logged
  `portal: stopping: the session bus closed the connection` within 3 ms of the kill,
  and `main` exits after that line. But the bus was the service's parent, so killing the
  bus orphans it, and whoever adopts an orphan has to reap it. On a desktop that is
  systemd or a subreaper. In a container it is PID 1, and GitHub creates job
  containers with `--entrypoint "tail"` and `"-f" "/dev/null"`, so PID 1 is `tail`,
  which never calls `wait`. The service stayed a zombie with parent 1, and its
  `/proc` entry never went. Reproduced in `archlinux:latest` created the way the
  runner creates it, with `tail -f /dev/null` as PID 1, steps through `docker exec`,
  `--cpus 4` and cargo as builder. 3 of 3 runs failed after 10 s, and each left one
  `delightfile <defunct>` under PID 1. The same test binary passed 3 of 3 in 0.2 s
  under `docker run --init`, whose tini reaps, and with bash as PID 1, which reaps
  whatever it adopts. A local run started as `docker run … bash <script>` passes for
  that reason. Timing and the builder user's environment played no part. The test
  now counts a zombie as exited: `has_exited` reads the `State:` line of
  `/proc/<pid>/status` and takes Z or X. A service that kept running still fails it,
  checked by making the service sleep after its bus closed: state S, and the test
  gave up as before. Nothing under `src/` changed, because the program does exit when
  its bus goes. `options: --init` on the job's container would also have turned the
  job green. It was not taken, because the test would stay wrong on any machine
  where nobody reaps orphans. With the change, the `tail` container passes 3 of 3,
  and so did the runner: run 36643463056 of `port/linux-ci` has the linux job green.
- 2026-09-29 — `app::tests::appearance::the_newer_surfaces_paint_from_the_light_palette`
  failed in 2 of the first 4 linux jobs on `port/linux-ci`, runs 36642159735 and
  36645262088, with `the permissions card's ripple painted a white splash on the
  light side: #03_03_03_03`. It never failed on Brian's machine: 0 of 50 runs alone,
  and 0 of 18 whole-suite runs pinned to 4, 2 and 1 CPUs. There was no white splash
  on that frame. The test switches to light, and the switch shows a "Light theme"
  toast whose plate fades in linearly over 220 ms (`RISE` in `toast.rs`) in latte's
  crust. Crust at alpha 1 to 3 premultiplies to `#01_01_01_01` through
  `#03_03_03_03`, the same bytes as white at that alpha. `no_white_splash` judged
  every colour on the frame, so a ripple frame painted 0.4 to 3 ms after the switch
  failed. The runner paints the frames in between fast enough to land there, and this
  machine does not. A toast born 2.5 ms before the ripple frame reproduces the exact
  message. There were three test-only ways out: clear the toast before the ripple
  checks, judge only what a ripple paints, or count r = g = b = a as white only from
  alpha 8 up. The second was taken. `painted` also returns the fill of every circle,
  which is all a ripple paints, since every `theme::splash` in df-app is a
  `circle_filled`, and `no_white_splash` judges only those. A light surface fading
  in is never taken for a splash, and a white splash is still caught at every alpha.
  Checked both ways. With the 2.5 ms toast on the ripple frame, `#03_03_03_03` is
  among the frame's colours, the old judgement fails and the new one passes. With
  `theme::splash` made to return white on the light side, the test fails on the
  permissions ripple with `#14_14_14_14`. Nothing outside the test helpers changed.
- 2026-09-30 — B6.5: `continue-on-error` is off both jobs. "Required" in the
  done-when is a branch-protection rule on `main` (Settings, Branches, require the
  `linux`, `macos` and `windows` checks), a repository setting only Brian can make;
  the workflow cannot. The tasks' own evidence is that all three jobs are green
  without it.
- 2026-09-30 — Lock files are two lines, the URL and then its sha256. B6.10's pin
  moved from the `windows` job's `env:` into `build/windows/ffmpeg.lock`, and
  `build/windows/fetch-ffmpeg.ps1` reads it for both workflows, so there is one pin.
  Each fetch script keeps its download under `target/`, where the release workflow
  caches the download alone, keyed on the lock's hash, and checks the sha256 on
  every run, cached or not. The Windows scripts unpack with
  `%SystemRoot%\System32\tar.exe` by path, because Git for Windows puts a GNU tar on
  `PATH` that reads `C:` as a host name. `ci.yml` fetches no pdfium: its tests pass
  either way (§2) and it packages nothing.
- 2026-09-30 — B6.14: `install.sh` takes what it installs from its own folder
  (`$here`, which is `build/` in the repository and the top folder in the tarball)
  and takes the binary beside itself when there is one, before
  `target/release/delightfile`. B6.14 had the order the other way round; this way
  a tarball unpacked inside a checkout installs its own binary, not the checkout's
  build. In the repository `build/delightfile` never exists, so every path
  `install.sh` reads or prints there is the same string as before. The guards
  B6.14 asked for were already there: `reload_bus` checks for `busctl` and ignores
  its failure, and `as_root` only calls `sudo` with a terminal. The tarball also
  carries `LICENSE` and `README.md`. Its binary links Arch's FFmpeg 9 and glibc, which
  README's Installing section says.
- 2026-09-30 — macOS minimum: `LSMinimumSystemVersion` is not B6.15's `13.0` but
  what the bundle can start on. `bundle.sh` computes it as the newest `minos`
  (`LC_BUILD_VERSION`) among the program and the libraries it carries, and the
  cask's `depends_on macos:` follows it. Homebrew builds each bottle for the macOS
  it is poured on, and on `macos-latest`, which is macOS 26, every Homebrew library
  in the bundle says `minos 26.0`. The program says 11.0 and pdfium 12.0. dyld
  refuses a library built for a newer macOS than the one running ("built for macOS
  26.0 which is newer than running OS"), so this bundle needs macOS 26. Writing
  `13.0` would ship an app that installs on 13 to 15 and dies at launch. Checked on
  the tahoe bottle of `ffmpeg` 9.0.2 with `llvm-otool -l` locally, and on the
  runner's own libraries in bundle.sh's log. How to lower it is in Open questions.
- 2026-09-30 — Homebrew removed `--no-quarantine`. It was deprecated in 4.7 and 5.0
  and is gone in 6.x (the runner has 6.0.22), with no replacement: what a cask
  installs is quarantined. The cask's caveats and README give
  `xattr -dr com.apple.quarantine /Applications/delightfile.app`, or Open Anyway
  under System Settings, Privacy & Security. B6.24's recommended command and §6's
  Homebrew row are corrected. A cask `postflight` that runs `xattr` still works in
  a third-party tap but is deprecated, and homebrew/cask rejects it, so the cask
  has none.
- 2026-09-30 — Both icons are drawn from `build/delightfile.svg` on the runner, not
  from `build/icons`' PNGs as B6.16 and B6.20 said, because the largest PNG is 256 px
  and a Mac icon wants 1024. On macOS, `bundle.sh` gives a copy of the SVG a 1024 px
  size, `sips` draws it (Quick Look is the fallback) and `iconutil` makes the
  `.icns`. On Windows, `build/windows/icon.ps1` has ImageMagick (7.1.2 on the
  windows-latest image) draw it at 768 px and scale it to 16 to 256 px in
  `delightfile.ico`. The `.ico` is generated and git ignores it. `build.rs` puts it
  in the exe when it exists and warns in a release build when it does not, and
  `package.ps1` refuses an exe without an icon.
- 2026-09-30 — B6.20's `crates/df-app/build.rs` runs a resource compiler only when
  the host and the target are both Windows: `RC` if set, `rc.exe` on `PATH`, the
  newest Windows Kits 10 x64 `rc.exe`, then `llvm-rc`. The version reaches the `.rc`
  through a header it writes into `OUT_DIR`, not `/d` defines, which would need
  quotes through two command-line parsers. The `.res` goes to the `delightfile`
  binary alone (`rustc-link-arg-bin`), not to test binaries. Elsewhere it prints
  `rerun-if-changed=build.rs` and returns, so Linux and macOS build what they built
  before, and `build/xcheck.sh` from Linux, which links nothing and has no `rc`,
  skips it.
- 2026-09-30 — The macOS release binary links with
  `-headerpad_max_install_names`, passed with `cargo rustc … -- -C link-arg=…`, so
  that `install_name_tool` can write the longer `@executable_path/../Frameworks/…`
  names into it. `cargo rustc` gives the flag to the final crate only, so no
  dependency rebuilds and no config file changes.
- 2026-09-30 — The release workflow checks every package a second time where
  nothing was built: `verify-linux` in a fresh `archlinux:latest` with only
  `ffmpeg`, `verify-macos` on a macOS runner without Homebrew's FFmpeg,
  `verify-windows` on a Windows runner that never fetched FFmpeg, and `publish`
  needs all three. The brew and scoop installs there point the cask and the
  manifest at the package on the runner (`file://`, a server on `127.0.0.1`),
  since during a dry run the release they name does not exist. Scoop's installer
  is pinned at commit `1e2f334`; Scoop itself installs from its default branch,
  which is acceptable in a check that builds nothing.
- 2026-09-30 — GitHub dispatches only a workflow it knows, one on the default
  branch or one that has run. `release.yml` is not on `main` yet, so it ran once on
  `port/release` from a temporary `push: branches: [port/release]` trigger (run
  36727782703), and the commit that carried the trigger was dropped from the branch
  (a force-push of `port/release`) before the dry runs by dispatch. That first run
  built all three packages and failed in `check-app.sh`, which took dyld 4's
  `dyld[pid]: <UUID> <path>` lines and its "move loaded to delayed" notes for paths
  outside the bundle; the check now reads the path after the UUID and skips lines
  that name none. Once `release.yml` is on `main`,
  `gh workflow run release.yml --ref <branch> -f dry_run=true` works for any branch.
- 2026-09-30 — Anything but a tag is a dry run, and a dispatch on a branch with
  `dry_run` off is refused. A release is a draft pre-release made by
  `gh release create --verify-tag`, so the workflow can never make the tag itself.
- 2026-09-30 — B6.23 on the first release: with no previous tag the notes list every
  commit subject, about 180 KB, over GitHub's 125,000-character limit on a
  release's text. The workflow cuts the list at 120,000 characters and says how
  many earlier commits it left out.
- 2026-09-30 — The workflow attaches the filled cask and manifest to the release and
  pushes nowhere. Brian creates `brianschwabauer/homebrew-tap` and copies the
  release's `delightfile.rb` to `Casks/delightfile.rb` there, and creates
  `brianschwabauer/scoop-bucket` and copies `delightfile.json` to
  `bucket/delightfile.json`. The step that would open those pull requests needs a
  `TAP_TOKEN` secret (a fine-grained token with contents and pull-request write on
  both repositories) and is not written: without the repositories there is nothing
  to test it against.
- 2026-09-30 — Scoop gives a windows-subsystem program a windows-subsystem shim
  ("Making …\shims\delightfile.exe a GUI binary"). A shell does not wait for one,
  so `delightfile --version` typed after a scoop install prints nothing where it
  was typed; the first dry run's check read an empty string from the shim. The
  window opens as it should, and the program itself prints, so the check now asks
  the installed exe and looks for the shim and the Start menu shortcut. Whether
  scoop should put the program's folder on `PATH` instead is in Open questions.
- 2026-09-30 — Decided (relayed by the coordinator): the release's macos job
  builds on `macos-15`; `ci.yml` stays on `macos-latest`. The bundle's minimum is
  whatever `macos-15`'s Homebrew bottles carry, `minos 15.0` in dry run
  36734749641, so the release starts on macOS 15 and later and the cask says
  `depends_on macos: :sequoia`. When GitHub retires the `macos-15` image, the
  minimum moves with whatever runner the job moves to. If a user asks for
  something older, option (c) in Open questions is the way back to 13: FFmpeg
  built from source with `MACOSX_DEPLOYMENT_TARGET=13.0`. `verify-macos` runs on
  `macos-15` and on `macos-latest`, the oldest macOS the bundle claims and the
  newest the runners have.
- 2026-09-30 — Decided: `build/macos/Info.plist` declares no document types until
  the app handles the folder Finder hands it, which is `02-macos.md` M2.36; that
  task puts `public.folder` back. `07-verification.md` §4.7 lists Open With from
  Finder as not available.
- 2026-09-30 — Decided: the macOS bundle (`Contents/Resources/Licenses`) and the
  Windows zip (`Licenses\` beside the exe) carry delightfile's license, FFmpeg's
  `COPYING.GPLv3`, pdfium's `LICENSE` with the licenses of what pdfium builds in,
  and `SOURCES.txt`. On macOS `COPYING.GPLv3` and `LICENSE.md` come from the
  Homebrew keg the bundled FFmpeg came from, and `SOURCES.txt` gives the
  formula's source URL and sha256 (read with `brew info --json=v2` and jq),
  refusing a keg whose version is not the formula's, and lists every other
  bundled Homebrew library with its version, license and source URL. Two things
  differ from what was decided, because BtbN's release is not what it was
  taken to be: its zip carries FFmpeg's license only as `LICENSE.txt` (byte for
  byte FFmpeg's `COPYING.GPLv3`, sha256 `8ceb4b9e…`), with no `LICENSE.md`, and
  its release page carries builds and checksums, no source archives. So the
  Windows `Licenses\FFmpeg\` holds `COPYING.GPLv3.txt` only, and `SOURCES.txt`
  names FFmpeg's repository at the commit BtbN's folder name ends in
  (`e47273f4d9`, which carries `LICENSE.md`) and BtbN's build scripts at the
  release `ffmpeg.lock` names, which pin every library built into the DLLs. The
  Windows files end in `.txt` so a double-click opens them. README's Installing
  says the downloads carry GPL FFmpeg and where `SOURCES.txt` is.
- 2026-09-30 — Decided: scoop keeps `bin`, and the manifest's `notes`, which
  scoop prints after an install, say that `--version` and `--help` print nothing
  through the shim, that the window opens as usual, and give the program's own
  path (`$dir\delightfile.exe`) for them.
- 2026-09-30 — FFmpeg's `LICENSE.md` in the Windows zip: option (c), a copy
  committed to the repository, decided for Brian, who delegated it.
  `build/windows/FFmpeg-LICENSE.md` is FFmpeg's `LICENSE.md` at `e47273f4d9`
  (`e47273f4d9227152dcbf543cebaf9e2430ddbcc4`, `n9.0.1-11-ge47273f4d9` on
  `release/9.0`), the commit the BtbN build in `ffmpeg.lock` is from. It was
  fetched from `https://git.ffmpeg.org/ffmpeg.git` and is 4,346 bytes, sha256
  `2e1d16c72fd74e12063776371da757322f8b77589386532f4fd8634bde7de1af`. It is
  the 9.0.1 release's text byte for byte. The blob is the one at the `n9.0.1`
  tag, and none of the eleven commits after the tag touches it. The bytes are
  those of the `LICENSE.md` in `ffmpeg-9.0.1.tar.xz` from
  `ffmpeg.org/releases`, whose signature is good from FFmpeg's release signing
  key (`FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8`). `package.ps1`
  copies it unchanged into the zip as `Licenses\FFmpeg\LICENSE.md.txt`, beside
  `COPYING.GPLv3.txt`. It refuses an FFmpeg build from any other commit until
  the file is replaced with that commit's `LICENSE.md` and
  `$ffmpeg_license_commit` in the script names the new commit.
  `check-package.ps1` looks for the file, and `SOURCES.txt` names it where it
  used to say the file was only in the source. The Windows `Licenses\FFmpeg\`
  now holds both of FFmpeg's files, as the macOS bundle does, and the entry
  above's "`COPYING.GPLv3.txt` only" no longer holds.

## Open questions

- Homebrew FFmpeg major on the macOS runner at implementation time (B6.7 resolves).
  (2026-09-29: `formulae.brew.sh` lists `ffmpeg` 9.0.2 with an `arm64_tahoe` bottle.
  The first run's FFmpeg step log settles it.) (2026-09-30: settled on the runner,
  main's run 36669749789 logs `Homebrew ffmpeg 9.0.1_1, libavcodec 63.1.101`.)
- Four tests pass on Brian's machine and fail in a clean archlinux container (found
  2026-09-29 in the local run under §1; each fails again when run alone). Until
  they are dealt with, the linux job goes red at `cargo test --workspace`. The two
  that needed a Nerd Font are settled in the Decisions log; these are the other
  two.
  - One depends on directory order. `app::places::tests::the_menus_pin_and_go`
    takes `sub`'s index from `entries()`, which came back as `["sub", "other"]`,
    and after `set_cursor` with it the row menu pins `other`. The test passes with
    TMPDIR on tmpfs, where the host's `/tmp` is, and fails with TMPDIR on btrfs
    and on the container's overlayfs.
  - One takes a branch the host never reaches.
    `archive::tests::a_zip_written_by_a_real_archiver_lists_the_same_way` builds
    its fixture with `zip` when it is installed and with `bsdtar` otherwise. The
    container has only bsdtar, which writes `./`-prefixed members (`./`,
    `./one.txt`, `./sub/`, `./sub/two.txt`), and the lister counts one of them
    unsafe: `unsafe_count()` is 1 where the test wants 0.

  There are two ways to settle them, and they can be combined. One is to change
  code under `crates/`. For the places test, take the cursor index from the order
  on screen. For the archive test, decide whether a `./` member is unsafe. The
  other is to make the container look like Brian's machine, adding `zip` to the
  pacman line. That turns the archive test green, but the places test still
  depends on the filesystem.
- The linux job uses Arch's `rust` package, while macOS and Windows use rustup's
  current stable. After a Rust release the two can differ by a version for
  a few days, and a new clippy lint under `-D warnings` would then fail one job and
  not the others. Should macOS and Windows pin the version Arch ships (for example
  `toolchain: 1.98`), and who bumps it?
- Bundle identifier `com.showandtour.delightfile`: confirm with Brian before the first
  release (it is baked into the notarization record and the app's preferences path).
- (2026-09-30) Which macOS the release starts on. Built as it is now, it needs macOS
  26 (Decisions log), where B6.15 and B6.24 planned 13 (Ventura). The ways to lower
  it:
  - (a) Keep Homebrew's FFmpeg. The minimum is the runner's macOS: 26 now, and it
    moves by itself when GitHub moves `macos-latest`, unless the release job pins
    `macos-26`. No work, no build time; the smallest audience.
  - (b) Build the release on `macos-15`, whose bottles say `minos 15.0`. One word in
    `release.yml`; macOS 15 and 26. The image is retired in a year or two, and then
    the minimum becomes (a)'s.
  - (c) Build FFmpeg for the release from source with
    `MACOSX_DEPLOYMENT_TARGET=13.0`, as §2.2's option 3 plus the target and
    `--enable-libdav1d` for AVIF (dav1d built the same way), cached by version. The
    minimum is 13.0 as planned, and the bundle drops what Homebrew's FFmpeg links
    and a previewer never uses (x264, x265, SVT-AV1, lame, openssl, libvpx's
    encoder). About ten minutes on a cache miss and the most to maintain.

  Decided 2026-09-30: (b), with (c) as the way back to 13 if a user asks
  (Decisions log).
- (2026-09-30) The bundle declares `public.folder` (B6.15), so Finder's Open With
  offers delightfile for a folder. But macOS hands the folder over as an
  open-documents event, and winit 0.30 does not pass that event on, so the app
  starts in its usual folder rather than the one chosen. Keep the declaration
  until df-app handles the event, or drop it until then?
  Decided 2026-09-30: dropped until `02-macos.md` M2.36 (Decisions log).
- (2026-09-30) Licenses in the packages. Each carries delightfile's `LICENSE` and
  pdfium's license files, and the Windows zip carries FFmpeg's `LICENSE.txt`. The
  macOS bundle does not carry the notices of Homebrew's FFmpeg and what it links
  (x264 and x265 are GPL, the rest BSD, LGPL or Apache). Distributing GPL FFmpeg
  binaries also means offering their source. How far should the release go here?
  Decided 2026-09-30: a `Licenses` folder with SOURCES.txt in both (Decisions log).
- (2026-09-30) Scoop and the command line. The manifest's `bin` makes a GUI shim,
  so `delightfile` from a terminal opens the window but `delightfile --version` and
  `--help` print nothing there (Decisions log). `"env_add_path": "."` in place of
  `bin` would put the real exe on `PATH`, which joins the terminal's console and
  prints (W4.1), at the cost of the FFmpeg DLLs' folder being on `PATH` too. Keep
  `bin`, or switch?
  Decided 2026-09-30: keep `bin`, and say so in the manifest's `notes`
  (Decisions log).
