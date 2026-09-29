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
`.github/workflows/ci.yml` holds the §1 matrix. It has not run yet, because nothing
has been pushed, so every task it covers is at most `[>]`.

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

- [>] **B6.1** Create `.github/workflows/ci.yml` triggered on `push` to `main` and on
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
- [>] **B6.2** Linux job runs in `container: archlinux:latest` with
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
- [>] **B6.3** macOS job on `macos-latest` (verify it is arm64 with `uname -m` in the
      log). Toolchain via `dtolnay/rust-toolchain@stable`. Native deps per §2.2.
      Done when: `cargo build -p df-core` and `cargo test -p df-core` pass on the
      runner. (df-app follows once Phase 1 stubs exist.) — written 2026-09-29,
      unverified until the first push. `uname -m` is logged and the step fails if it
      is not `arm64`; the action is pinned by commit with `toolchain: stable`.
- [>] **B6.4** Windows job on `windows-latest`, MSVC toolchain. Native deps per
      §2.3. Same done-when as B6.3. — written 2026-09-29, unverified until the first
      push. Toolchain `stable-x86_64-pc-windows-msvc`; `core.autocrlf false` before
      checkout.
- [ ] **B6.5** Remove `continue-on-error` from the macOS and Windows jobs. This is the
      last task of Phase 1 (`01-platform-seam.md` S1.40 cross-references it).
      Done when: all three jobs are required and green on `main`.
- [>] **B6.6** Add `cargo fmt --check` to the linux job only. Done when: green.
      — written 2026-09-29, passes in a local archlinux container, unverified on
      GitHub until the first push. It is the first cargo step in the job, straight
      after the ownership hand-over.

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

- [>] **B6.7** Establish how to get FFmpeg with **libavcodec major 63** on the macOS
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
      option 2 or 3 is below.
- [ ] **B6.8** pdfium for macOS: download `pdfium-mac-arm64.tgz` from the
      `bblanchon/pdfium-binaries` release matching the `pdfium_7881` ABI feature in
      `crates/df-app/Cargo.toml` (chromium/7881 or the nearest release that keeps the
      ABI; the pdfium-render docs list the pairing). Store the URL and sha256 in
      `build/macos/pdfium.lock` (a two-line text file). The runner fetches and verifies
      it. Done when: the file exists and the fetch step verifies the hash.
      Researched 2026-09-29: the release exists, and its URL and sha256 are in the
      Decisions log. The lock file is not written yet.
- [>] **B6.9** Confirm `libclang` is available for `bindgen` on the runner (Xcode's
      is). Done when: `dv-media` builds without a `LIBCLANG_PATH` override, or the
      override is set in the workflow with a comment. — written 2026-09-29,
      unverified until the first push. The workflow sets no override. The Runner step
      logs `xcode-select -p`, the Xcode whose toolchain clang-sys searches for
      libclang. If bindgen cannot find libclang, add to the FFmpeg step:
      `echo "LIBCLANG_PATH=$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib" >> "$GITHUB_ENV"`.

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

- [>] **B6.10** FFmpeg for Windows: download a `ffmpeg-n9.*-win64-gpl-shared` build
      from `BtbN/FFmpeg-Builds` releases (GPL matches the project licence). Pin the
      exact asset name and sha256 in `build/windows/ffmpeg.lock`. The workflow
      extracts it and sets `FFMPEG_DIR` to the extracted directory, which is what
      `ffmpeg-sys-next` reads on Windows. Done when: `cargo build -p dv-media` passes
      on the runner. — written 2026-09-29, unverified until the first push. The pin
      (`FFMPEG_URL`, `FFMPEG_SHA256`) is in the `windows` job's `env:` in
      `ci.yml` for now: `build/` was outside that change, so
      `build/windows/ffmpeg.lock` does not exist yet. The step checks the sha256,
      extracts with `7z`, fails unless `version_major.h` says 63, and sets
      `FFMPEG_DIR`.
- [>] **B6.11** `bindgen` on Windows needs `LIBCLANG_PATH`. The `windows-latest`
      image has LLVM under `C:\Program Files\LLVM`; set the variable in the workflow.
      Done when: build passes. — written 2026-09-29, unverified until the first
      push. The image's LLVM is 20.1.8, installed by Chocolatey into that directory.
      The step falls back to Visual Studio's bundled LLVM (`VC\Tools\LLVM\x64\bin`
      via `vswhere`), which is what rust-ffmpeg's own Windows CI uses, when
      `libclang.dll` is not there.
- [ ] **B6.12** pdfium for Windows: `pdfium-win-x64.tgz` from the same
      `bblanchon/pdfium-binaries` release as B6.8, pinned in
      `build/windows/pdfium.lock`. Done when: fetched and verified in the job.
      Researched 2026-09-29: the release exists, and its URL and sha256 are in the
      Decisions log. The lock file is not written yet.
- [>] **B6.13** The `windows` job's smoke test must run with the FFmpeg `bin/`
      directory on `PATH` so the DLLs resolve. Done when: `delightfile --version`
      prints on the runner. — written 2026-09-29, unverified until the first push.
      The FFmpeg step appends `bin\` to `GITHUB_PATH`, so the df-app tests get it
      too.

**Switching B6.10 to another build** (researched 2026-09-29). The step derives the
directory name from the URL (the zip's top directory is the asset name without
`.zip`, for BtbN's builds and gyan.dev's alike), so a switch changes only
`FFMPEG_URL` and `FFMPEG_SHA256`:

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

- [ ] **B6.14** `build/linux/package.sh` produces
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

### 3.2 macOS (`build/macos/`)

- [ ] **B6.15** `build/macos/Info.plist` with `CFBundleIdentifier`
      `com.showandtour.delightfile` (Brian's domain; change only if he says so),
      `CFBundleExecutable delightfile`, `CFBundleName delightfile`,
      `CFBundleIconFile delightfile.icns`, `CFBundleShortVersionString` and
      `CFBundleVersion` filled from `Cargo.toml` at package time, `LSMinimumSystemVersion`
      `13.0`, `NSHighResolutionCapable true`, `CFBundleDocumentTypes` declaring
      `public.folder` so Finder offers the app for folders, and
      `NSSupportsAutomaticGraphicsSwitching true`. Done when: `plutil -lint` passes.
- [ ] **B6.16** `build/macos/bundle.sh`: assembles `delightfile.app` from
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
- [ ] **B6.17** `build/macos/dmg.sh`: `hdiutil create` a compressed dmg
      `delightfile-<version>-aarch64-macos.dmg` containing the `.app` and an
      `Applications` symlink. Done when: the dmg mounts and the app inside passes
      B6.16's checks.
- [ ] **B6.18** The app must find `libpdfium.dylib` in `Contents/Frameworks` at
      runtime: Phase 1/2 adds that search path in the pdfium loader
      (`02-macos.md` cross-references). Done when: PDF preview works in the bundled
      app on a real Mac (`07-verification.md` V7.x) — until then, done at "the loader
      logs the path it tried".

### 3.3 Windows (`build/windows/`)

- [ ] **B6.19** `build/windows/package.ps1` produces
      `delightfile-<version>-x86_64-windows.zip` containing `delightfile.exe`, the
      FFmpeg `bin/*.dll` set from B6.10 (only the libraries the exe imports: check with
      `dumpbin /dependents`, and their transitive deps), `pdfium.dll`, and a
      `README.txt` with the SmartScreen note from §6. Done when: extracting the zip
      on a clean `windows-latest` runner and running `delightfile.exe --version`
      works without any other install.
- [ ] **B6.20** Icon and version resource: `build/windows/delightfile.rc` referencing
      `delightfile.ico` (generated from the PNG icons with ImageMagick or a small
      script in `build/windows/`) and a `VERSIONINFO` block. `crates/df-app/build.rs`
      compiles it with `rc.exe` (always present with the MSVC toolchain) only when
      `CARGO_CFG_WINDOWS` is set, and emits `cargo:rustc-link-arg=<path>.res`. No
      `winres`/`embed-resource` crate. Done when: Explorer shows the icon on the exe
      and `--version`'s number matches the file properties.
- [ ] **B6.21** `#![windows_subsystem = "windows"]` in `main.rs` gated on
      `cfg(windows)` so no console opens, and `--version`/`--help` still print by
      attaching to the parent console (`AttachConsole(ATTACH_PARENT_PROCESS)` via
      `windows-sys`, attempted unconditionally at startup — it fails harmlessly when
      the app was launched from Explorer). This is a Phase 4 code
      task (`04-windows.md` W4.1); it is listed here because the packaging smoke test
      depends on it. Done when: B6.19's smoke test prints the version in the runner log.

## 4. Release workflow

- [ ] **B6.22** `.github/workflows/release.yml` on tags `v*`: a `check-version` job
      that fails unless the tag equals `v` + the workspace version; then the three
      build jobs reusing the CI steps and the §3 scripts; then a `publish` job that
      creates a GitHub Release with the three artifacts and their `.sha256` files,
      marked pre-release until Brian edits it. Done when: a dry-run on a `v0.0.0-test`
      tag on a branch produces all three artifacts (delete the tag afterwards).
- [ ] **B6.23** Release notes: the workflow drafts them from the commit subjects
      since the previous tag. Brian edits before publishing. Done when: the draft
      appears.

## 5. Distribution channels

- [ ] **B6.24** Homebrew tap: a repository `brianschwabauer/homebrew-tap` with
      `Casks/delightfile.rb` pointing at the dmg URL and sha256, `depends_on macos:
      ">= :ventura"`, and a `caveats` block with the unsigned-app instructions (§6):
      the recommended command is `brew install --no-quarantine --cask
      brianschwabauer/tap/delightfile`, because Homebrew **does** quarantine cask
      downloads by default and only `--no-quarantine` (or
      `HOMEBREW_CASK_OPTS=--no-quarantine`) skips Gatekeeper's dialog.
      The release workflow opens a PR against the tap bumping version and sha256
      (needs a `TAP_TOKEN` secret with contents write on that repo). Brian creates the
      repository and the token; the agent writes the cask and the workflow step.
      Done when: `brew install --cask brianschwabauer/tap/delightfile` installs on a
      Mac (`07-verification.md`), or until then, `brew audit --cask` passes on the
      runner.
- [ ] **B6.25** scoop bucket: repository `brianschwabauer/scoop-bucket` with
      `bucket/delightfile.json` (`url`, `hash`, `bin`, `checkver` on GitHub releases,
      `autoupdate`). Release workflow bumps it the same way. Done when: `scoop bucket
      add` + `scoop install delightfile` works on the runner.
- [ ] **B6.26** README: an "Installing" section per platform with the exact commands
      and the unsigned-app notes. Done when: reviewed by Brian.
- [~] **B6.27** winget manifest — skipped until there is a signed build or a user asks;
      winget's review process is slow and a `[~]` here keeps the option visible.

## 6. The signing decision

What "unsigned" costs users:

| Platform | First launch experience | Workaround the README documents |
|----------|------------------------|--------------------------------|
| macOS 14 | Gatekeeper: "cannot be opened because the developer cannot be verified"; right-click → Open works | Right-click, Open, Open |
| macOS 15+ | Same dialog but right-click → Open no longer bypasses it | System Settings → Privacy & Security → scroll to "delightfile was blocked" → Open Anyway; or `xattr -d com.apple.quarantine /Applications/delightfile.app` |
| Homebrew cask | Homebrew quarantines cask downloads by default, so a plain `brew install --cask` hits the same dialog as the dmg | `brew install --no-quarantine --cask …`; the caveats say so |
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
  `01-platform-seam.md`). Off Linux, df-app has some 48 dead-code warnings per
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

## Open questions

- Homebrew FFmpeg major on the macOS runner at implementation time (B6.7 resolves).
  (2026-09-29: `formulae.brew.sh` lists `ffmpeg` 9.0.2 with an `arm64_tahoe` bottle.
  The first run's FFmpeg step log settles it.)
- Four tests pass on Brian's machine and fail in a clean archlinux container (found
  2026-09-29 in the local run under §1; each fails again when run alone). Until
  they are dealt with, the linux job goes red at `cargo test --workspace`.
  - Two need a Nerd Font. `icons::glyph_tests::the_chrome_glyphs_all_render`
    panics with "no face draws '✓' (U+2713)", and
    `menu::tests::a_key_stands_clear_of_the_chevron` with "the key is -0.84375 pt
    from the ▸". On the host, hiding `/usr/share/fonts/TTF` and
    `~/.local/share/fonts` in a mount namespace reproduces both. The glyph test's
    own doc says the plain glyphs must draw "patched font or not". So on a machine
    without a Nerd Font, which includes every stock Mac and Windows install, the
    chrome has at least ✓ drawn as a missing-glyph box. That matters to the port
    as well as to CI.
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
  code under `crates/`. For the fonts, decide what the chrome falls back to for ✓
  and ▸. For the places test, take the cursor index from the order on screen. For
  the archive test, decide whether a `./` member is unsafe. The other is to make
  the container look like Brian's machine, adding a Nerd Font such as
  `ttf-jetbrains-mono-nerd` (232 MiB installed) and `zip` to the pacman line. That
  turns three of the four green, but it hides the font gap from CI, and the places
  test still depends on the filesystem.
- The linux job uses Arch's `rust` package, while macOS and Windows use rustup's
  current stable. After a Rust release the two can differ by a version for
  a few days, and a new clippy lint under `-D warnings` would then fail one job and
  not the others. Should macOS and Windows pin the version Arch ships (for example
  `toolchain: 1.98`), and who bumps it?
- Bundle identifier `com.showandtour.delightfile`: confirm with Brian before the first
  release (it is baked into the notarization record and the app's preferences path).
