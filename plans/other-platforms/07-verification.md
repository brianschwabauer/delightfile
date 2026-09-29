# 07 — Verification

Status: **not started**

Scope: what the automated pipeline proves, what only a human at a real machine can
prove, the per-platform live checklist, and the rule that gates a release on it.
This document is filled in as the other phases land; a phase document that marks a
UI-visible task done at "compiles and unit tests pass" must add the on-screen check
here at the same time.

## 1. What CI proves

For each of Linux, macOS and Windows, on every push (`06-build-and-release.md` §1):

- the workspace compiles, so the platform seam has the same function set everywhere;
- `cargo test --workspace` passes, which on macOS and Windows means: all of
  df-core's headless tests (fs model, sort, filter, ops journal, tasks, keymap,
  config, rename templates, archive readers, git, du, zoxide) and df-app's
  `App::for_test` fixture tests that never open a window;
- clippy is clean;
- the built binary loads its shared libraries and answers `--version`.

That is a lot, and it is also the same "built and unit-tested but no human drove the
window" state that this project has learned not to trust (the btrfs cursor-order bug,
the stale portal wrapper, the picker's double-click bypass were all found by eyes on
a screen). Nothing below §1 is CI's job.

## 2. What CI cannot prove

- Anything a window does: first frame, HiDPI scaling, present mode behaviour,
  occlusion (`graphics.rs` has a comment that its occlusion handling is dead on
  Wayland and live on macOS; nobody has seen it live).
- Keyboard conventions on macOS: Cmd-as-Ctrl, Option-letter bindings, arrow
  bindings surviving the Option composition rules.
- Every native integration: file watching latency and coalescing, trash and its undo,
  clipboard both directions against Finder/Explorer, drag-in and drag-out against
  Finder/Explorer, openers, terminal-at, the volumes list, unmount/eject, SFTP over
  the platform's `ssh`.
- Video and audio: cpal output device selection, Metal/DX12 texture upload of NV12
  frames, the shuttle keys.
- Fonts: whether the icon font is found, whether text renders with the platform's
  fallback fonts for CJK and emoji.
- Packaging: the `.app` launches from `/Applications` via Finder (not only from a
  terminal), Gatekeeper's behaviour matches §6 of the build document, the zip's DLLs
  resolve when launched by double-click from Explorer.

## 3. Getting a machine

Not a task for an agent; recorded here so the constraint is visible.

- **macOS**: the port cannot be finished without one. Options, cheapest first: a
  used Apple Silicon Mac mini (M1, 8 GB is enough); an AWS EC2 `mac2.metal`
  instance (24-hour minimum allocation, hourly rate, fine for a weekend of testing);
  MacStadium or similar hourly rental; a friend's machine with a USB stick. GitHub's
  macOS runners cannot show a window.
- **Windows**: a Windows 11 VM on the Linux machine is enough for functional testing.
  wgpu's DX12 backend runs on WARP (Microsoft's software rasterizer) when no GPU is
  passed through, so the app will run, slowly; Phase 4 must make sure the adapter
  request accepts a fallback adapter on Windows (`04-windows.md` cross-references).
  GPU passthrough of the NVIDIA card is the upgrade if the VM is too slow to judge
  motion.

## 4. Live checklist — macOS

Run on real hardware, from the `.app` produced by the release workflow (not a
`cargo run`), once per release candidate. Tick here with the date and the macOS
version. Each line is one `[ ]`; a failure gets a `[~]` and an issue in the phase
document that owns the feature.

### 4.1 Launch and window
- [ ] Double-click the app in `/Applications`: window appears within a second, Dock
      icon is the delightfile icon, no console.
- [ ] Gatekeeper behaviour matches `06-build-and-release.md` §6 for the installed
      macOS version. Record the exact dialog text.
- [ ] Retina scaling: text is crisp, the window's logical size matches a Linux window
      of the same configured size, `-`/`=` view-scale ladder looks the same.
- [ ] Fullscreen (green button) and back; resize; minimize to Dock and restore
      (this exercises the occlusion path that is dead on Wayland).
- [ ] Two windows via Ctrl+N (or Cmd+N per Phase 5's mapping): two processes, two
      Dock icons, both responsive; closing one leaves the other.
- [ ] Dark and light appearance: the theme is whatever the config says; no unreadable
      combination (the app does not follow system appearance unless Phase 5 says so).
- [ ] Quit via `q`, via Cmd+Q, via the red button: state file written, no zombie
      process (`ps aux | grep delightfile`).

### 4.2 Keyboard
- [ ] Arrows, Enter, Backspace, Space, Tab, Esc, `?` help sheet.
- [ ] Every default binding that uses Alt on Linux works with Option on macOS
      without inserting a composed character (Alt+Left/Right word motion in the
      rename prompt, Alt+Up/Down in bulk rename, Alt+- zoom, Alt+p).
- [ ] Every default Ctrl binding works with the modifier Phase 5 chose (Ctrl or Cmd,
      or both). Ctrl+L go-to-path, Ctrl+a, Ctrl+r, Ctrl+d/Ctrl+Shift+l in bulk rename,
      Ctrl+z, Ctrl+wheel view scale, Ctrl+←/→ page turn in a PDF.
- [ ] Typing in a prompt: `~`, `/`, `-`, `[`, `]`, non-ASCII letters, an emoji via
      the character viewer.
- [ ] Caps Lock on: `j`/`k` still shuttle, `J`/`K` are not sent.

### 4.3 Listing and filesystem
- [ ] `~`, `/`, `/Volumes`, `/System` (permission-denied handling), a folder with
      10,000 entries (scan time, scroll).
- [ ] Case-insensitivity: rename `Foo` → `foo` succeeds and the row updates; creating
      `foo` next to `Foo` is refused with the conflict card.
- [ ] Watch: create, delete and rename files in the current folder from Finder and
      from a terminal; each change appears without a manual refresh within the
      watcher's debounce (kqueue, Phase 2). Delete the folder being viewed: the app climbs to the
      parent as on Linux.
- [ ] Size column and `m u` disk usage on an APFS volume with clones: numbers agree
      with `du -sh` within reason; the `≈` marker appears while walking.
- [ ] Git chip on a repository.
- [ ] Symlink display, symlink creation, hard link creation (unbound commands via the
      palette).
- [ ] `.DS_Store` and `._*` files: hidden-file toggle treats them as hidden files
      (dot-prefixed), nothing special.

### 4.4 Operations
- [ ] Copy within a volume (expect `clonefile`, instant), copy across volumes
      (chunked), move within and across volumes, the conflict card, cancel mid-copy
      via the task panel.
- [ ] Trash with `d`: file appears in Finder's Trash; `u` undo restores it to the
      original folder; `D` deletes permanently after the prompt; the trash view tab
      lists, restores and purges items the app trashed.
- [ ] Items trashed by Finder do not appear in the trash view (M2.8 decided it:
      the view lists what the app trashed, from its journal); emptying the Trash in
      Finder takes the app's items out of the view too.
- [ ] Tags (`T`): a tag set in the app shows in Finder, a colour tag in its colour;
      a tag set in Finder shows on the row; a copy (same volume and across) keeps
      them; `T` on a symlink is refused (M2.33).
- [ ] Owner linemode (`m o`): the logged-in user and `staff` by name (M2.4).
- [ ] Rename, bulk rename with `{taken}` on a photo (EXIF read), `{camera}`.
- [ ] Create file, create folder with `/` in the prompt.
- [ ] Archive: extract a zip, a tar.gz, a 7z (if `7z` is installed via Homebrew,
      otherwise expect the toast), `A` compress.

### 4.5 Clipboard and drag
- [ ] `y` in the app, Cmd+V in Finder: files paste.
- [ ] Cmd+C in Finder, `p` in the app: files paste; the toast names the count.
- [ ] `Y` (copy path) then paste into TextEdit: the path text.
- [ ] Drag a file from Finder onto the list: drop-in copies (or moves with the
      modifier Phase 2 chose); the drop highlight tracks the pointer.
- [ ] Drag a row out of the app onto the Desktop: a copy appears; onto a Mail
      compose window: an attachment; onto the other delightfile window: a copy.
- [ ] Escape mid-drag cancels.

### 4.6 Preview
- [ ] JPEG, PNG, WebP, animated GIF, HEIC from an iPhone (through FFmpeg), a 4K
      video with audio (play toggles on click, `j`/`k`/`l` shuttle, mute with Ctrl+m),
      an MP3 with a sleeve, a PDF (page turn), a markdown file with highlighting, a
      zip listing, a `.ttf` specimen sheet, a 3MF.
- [ ] Zoom and pan gestures with a trackpad (pinch, two-finger scroll, double-click).
- [ ] Grid view thumbnails populate and are found again after a restart (cache at the
      path Phase 5 chose).

### 4.7 Integration
- [ ] Enter on a file opens it in the default app; `o` shows the opener picker with
      the macOS defaults from Phase 5; `terminal-at` opens Terminal (or ghostty when
      installed) in the folder.
- [ ] `M` volumes card: internal disk, a USB stick, a mounted dmg, a network share;
      eject the USB stick from the card and confirm Finder agrees.
- [ ] SFTP: `Connect to:` an ssh host that is in `~/.ssh/config`; list, preview a
      text file, copy a file down, copy a file up.
- [ ] A cloud remote open, then `kill -9` the app: `rclone rcd` is gone within a
      second (`ps aux | grep rclone`) (M2.29).
- [ ] `alt+p` sync to a server with Apple's `rsync` only: refused, and the toast
      names Homebrew's (`RSYNC_HINT`); with `brew install rsync`: it runs (M2.6).
- [ ] `--chooser-file` and the portal flag are absent from `--help`.
- [ ] Zoxide jumps if `zoxide` is installed via Homebrew.

## 5. Live checklist — Windows

Run on real hardware or the VM from §3, from the zip produced by the release
workflow, launched by double-click from Explorer.

### 5.1 Launch and window
- [ ] Double-click `delightfile.exe`: window within a second, no console window, the
      taskbar icon is the delightfile icon. SmartScreen dialog text recorded.
- [ ] `delightfile.exe --version` from a `cmd` and from PowerShell prints (console
      attach works).
- [ ] 125 % and 150 % display scaling: crisp text, correct hit targets.
- [ ] Maximize, snap to half screen, minimize and restore.
- [ ] Two windows via Ctrl+N.
- [ ] Quit via `q`, via Alt+F4, via the title bar ×.

### 5.2 Keyboard
- [ ] All the Linux default bindings with Ctrl and Alt; AltGr on a German layout
      does not fire Alt bindings when typing `@` or `€` in a prompt.
- [ ] Windows key does nothing unexpected.

### 5.3 Paths and listing
- [ ] `C:\`, `C:\Users\<name>`, `D:\` (a second drive), a UNC path
      `\\server\share`, a path over 260 characters, a folder named `con` (reserved)
      shows and is not enterable-as-file, a file named `Foo` and `foo` cannot coexist.
- [ ] Breadcrumb shows drive roots as Phase 3 specified (`C:` then segments), the
      `Go to:` prompt accepts both `C:\Users` and `C:/Users`, the `~` shortcut goes
      to the user profile.
- [ ] Watch: create, delete, rename from Explorer and from PowerShell; each appears
      without refresh.
- [ ] Size column, `m u` usage, git chip, hidden files (attribute-hidden and
      dot-prefixed both count as hidden per Phase 3's decision).
- [ ] Symlink creation: refused with the privilege toast when not elevated, works
      with Developer Mode on; junctions display as links.

### 5.4 Operations
- [ ] Copy, move, within and across drives, conflict card, cancel.
- [ ] Trash with `d`: item in the Recycle Bin; `u` after trash does what Phase 4
      decided (restore via the journal or a toast pointing at the Recycle Bin); `D`
      permanent; the trash view tab does what Phase 4 decided.
- [ ] Rename with a case-only change; bulk rename.
- [ ] Extract zip, tar.gz; 7z when `7z.exe` is on `PATH`.

### 5.5 Clipboard and drag
- [ ] `y` then Ctrl+V in Explorer; Ctrl+C in Explorer then `p`; `x` then paste in
      Explorer moves (the `Preferred DropEffect` format).
- [ ] Drag from Explorer into the list.
- [ ] Drag out of the app: works if Phase 4 implemented OLE drag-out; otherwise the
      row does not start a drag and the plan's `[~]` is referenced here.

### 5.6 Preview
- [ ] Same media list as macOS §4.6 with the DX12 backend; on the VM with WARP,
      note the frame rate but judge correctness only.
- [ ] Audio output through the default device; changing the default device
      mid-playback does not crash.

### 5.7 Integration
- [ ] Enter opens with the file association; `o` shows the Windows opener defaults;
      `terminal-at` opens Windows Terminal (or `cmd` when `wt` is absent) in the
      folder.
- [ ] `M` drives card lists every drive with type and free space; eject a USB stick
      if Phase 4 implemented it.
- [ ] SFTP through the built-in OpenSSH client.

## 6. Release gate

- [ ] **V7.1** No macOS artifact is attached to a public (non pre-release) GitHub
      Release until §4 has been run once on real hardware with every line ticked or
      `[~]`-logged, and the run is recorded here with date, machine and macOS version.
- [ ] **V7.2** The same for Windows and §5.
- [ ] **V7.3** The Linux artifact is gated on Brian's daily use, as today, plus the
      CI job.
- [ ] **V7.4** After each live run, every `[~]` line becomes either a task in the
      owning phase document or a Decisions-log entry accepting the gap.

## Decisions log

- 2026-09-25 — Release gate requires one human pass per platform; CI is necessary,
  not sufficient.

## Open questions

- Which machine Brian will use for the macOS pass (§3).
