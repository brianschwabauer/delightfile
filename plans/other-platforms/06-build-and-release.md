# 06 — Build and release

Status: **not started**

Scope: the CI matrix that keeps all three targets compiling, the release workflow
that produces installable artifacts, the packaging of native libraries the app needs
at runtime (FFmpeg, pdfium), distribution channels (Homebrew tap, scoop), and the
signing decision. This document runs alongside Phase 1: the CI job (§1) must exist
before Phase 1 can be marked done, because it is the only enforcement of the platform
seam. Everything from §3 on waits for Phase 2 (macOS) or Phase 4 (Windows) to have
something worth shipping.

Today there is no `.github/` directory, no CI, and no release process: Brian builds
with `cargo build --release` and `build/install.sh` (see
`appendix-inventory-df-app.md` §5 for what that script does).

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

- [ ] **B6.1** Create `.github/workflows/ci.yml` triggered on `push` to `main` and on
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
      it).
- [ ] **B6.2** Linux job runs in `container: archlinux:latest` with
      `pacman -Syu --noconfirm rust ffmpeg pkgconf clang git`. Cache
      `~/.cargo/registry`, `~/.cargo/git`, `target/` keyed on `Cargo.lock` and the
      job name.
      Done when: a cold run and a warm run both pass and the warm run is under 10
      minutes.
- [ ] **B6.3** macOS job on `macos-latest` (verify it is arm64 with `uname -m` in the
      log). Toolchain via `dtolnay/rust-toolchain@stable`. Native deps per §2.2.
      Done when: `cargo build -p df-core` and `cargo test -p df-core` pass on the
      runner. (df-app follows once Phase 1 stubs exist.)
- [ ] **B6.4** Windows job on `windows-latest`, MSVC toolchain. Native deps per
      §2.3. Same done-when as B6.3.
- [ ] **B6.5** Remove `continue-on-error` from the macOS and Windows jobs. This is the
      last task of Phase 1 (`01-platform-seam.md` S1.40 cross-references it).
      Done when: all three jobs are required and green on `main`.
- [ ] **B6.6** Add `cargo fmt --check` to the linux job only. Done when: green.

## 2. Native dependencies per target

### 2.1 Linux

System FFmpeg 9 via pacman. pdfium optional at runtime as today. Nothing changes.

### 2.2 macOS

- [ ] **B6.7** Establish how to get FFmpeg with **libavcodec major 63** on the macOS
      runner. Try in order and record the winner in the Decisions log:
      1. `brew install ffmpeg` then `pkg-config --modversion libavcodec`; use it if the
         major is 63.
      2. A versioned formula (`ffmpeg@9`) if Homebrew has one by then.
      3. Build FFmpeg from source into `$HOME/ffmpeg-prefix` with
         `--enable-shared --disable-static --disable-programs --disable-doc
         --enable-gpl` and the decoders the app uses, cached by the FFmpeg version
         string; export `PKG_CONFIG_PATH=$HOME/ffmpeg-prefix/lib/pkgconfig`.
      Done when: `cargo build -p dv-media` succeeds on the runner and the log shows
      which option was used.
- [ ] **B6.8** pdfium for macOS: download `pdfium-mac-arm64.tgz` from the
      `bblanchon/pdfium-binaries` release matching the `pdfium_7881` ABI feature in
      `crates/df-app/Cargo.toml` (chromium/7881 or the nearest release that keeps the
      ABI; the pdfium-render docs list the pairing). Store the URL and sha256 in
      `build/macos/pdfium.lock` (a two-line text file). The runner fetches and verifies
      it. Done when: the file exists and the fetch step verifies the hash.
- [ ] **B6.9** Confirm `libclang` is available for `bindgen` on the runner (Xcode's
      is). Done when: `dv-media` builds without a `LIBCLANG_PATH` override, or the
      override is set in the workflow with a comment.

### 2.3 Windows

- [ ] **B6.10** FFmpeg for Windows: download a `ffmpeg-n9.*-win64-gpl-shared` build
      from `BtbN/FFmpeg-Builds` releases (GPL matches the project licence). Pin the
      exact asset name and sha256 in `build/windows/ffmpeg.lock`. The workflow
      extracts it and sets `FFMPEG_DIR` to the extracted directory, which is what
      `ffmpeg-sys-next` reads on Windows. Done when: `cargo build -p dv-media` passes
      on the runner.
- [ ] **B6.11** `bindgen` on Windows needs `LIBCLANG_PATH`. The `windows-latest`
      image has LLVM under `C:\Program Files\LLVM`; set the variable in the workflow.
      Done when: build passes.
- [ ] **B6.12** pdfium for Windows: `pdfium-win-x64.tgz` from the same
      `bblanchon/pdfium-binaries` release as B6.8, pinned in
      `build/windows/pdfium.lock`. Done when: fetched and verified in the job.
- [ ] **B6.13** The `windows` job's smoke test must run with the FFmpeg `bin/`
      directory on `PATH` so the DLLs resolve. Done when: `delightfile --version`
      prints on the runner.

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

## Open questions

- Homebrew FFmpeg major on the macOS runner at implementation time (B6.7 resolves).
- Bundle identifier `com.showandtour.delightfile`: confirm with Brian before the first
  release (it is baked into the notarization record and the app's preferences path).
