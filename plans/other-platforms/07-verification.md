# 07 — Verification

Status: **in progress**

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
- [ ] Dark and light appearance: with `[flavor] mode = "dark"` or `"light"` the
      window stays on that side whatever System Settings says; no unreadable
      combination.
- [ ] `[flavor] mode = "auto"` (M2.30): the first frame is on the side System
      Settings → Appearance is on; switching Appearance turns the window at once,
      including a window left idle with nothing animating; `theme-auto` from the
      palette toasts "Following the desktop (light)" or "(dark)", never "Could not
      reach…".
- [ ] Quit via `q`, via Cmd+Q, via the red button: state file written, no zombie
      process (`ps aux | grep delightfile`).
- [ ] Cmd+Q from a shell wrapper that reads `--cwd-file` (M2.26): the shell lands
      in the folder the window was showing, as it does after `q` and after the red
      button; after `Q` it stays where it was.
- [ ] Minimize to the Dock and restore with `RUST_LOG=debug` (M2.23): the log says
      "surface occluded; frames paused until the window is shown" once, then
      "surface visible again" once; the restored window is current at once.
- [ ] Scroll a long listing and flick through a folder of pictures (M2.24): motion
      is even under `Fifo`, with no stutter a Linux window of the same content
      does not have.
- [ ] Cmd+N (M2.25): a second `delightfile` process and a second Dock tile, both
      from inside the `.app`.
- [ ] With `brew install --cask font-symbols-only-nerd-font` (M2.19): row icons are
      the Nerd Font's glyphs; without it, the `ls`-style classifiers, and the log
      says "no Nerd Font found".

### 4.2 Keyboard
- [ ] Arrows, Enter, Backspace, Space, Tab, Esc, `?` help sheet.
- [ ] Every default binding that uses Alt on Linux works with Option on macOS
      without inserting a composed character (Alt+Left/Right word motion in the
      rename prompt, Alt+Up/Down in bulk rename, Alt+- zoom, Alt+p).
- [ ] Every default Ctrl binding works with the modifier Phase 5 chose (Ctrl or Cmd,
      or both). Ctrl+L go-to-path, Ctrl+a, Ctrl+r, Ctrl+d/Ctrl+Shift+l in bulk rename,
      Ctrl+z, Ctrl+wheel view scale, Ctrl+←/→ page turn in a PDF.
- [ ] Command and Control are one modifier (M2.20): each of the chords above fires
      with Cmd and with Control alike; Cmd+P opens the palette, Cmd+N a window.
- [ ] Cmd means what a Mac means (D5.5): in the list, Cmd+C copies the selection's
      paths (paste into TextEdit), Cmd+W closes the tab (the last one quits),
      Cmd+V pastes (the yank, else the files copied in Finder) and Cmd+Q quits;
      in a rename prompt Cmd+C copies the selected text, or the whole name with
      nothing selected, and leaves the prompt open; in the palette and every
      card Cmd+C still closes it.
- [ ] Chord labels (M2.21): the help sheet, the which-key card, the menus and
      the palette write chords with `⌃ ⌥ ⇧ ⌘` run together in that order —
      `⌥←`, `⇧⌘Z`, `⌘p` — and never `Ctrl+`; `⌥` and `⌃`, which the app draws
      itself, sit on the baseline beside the letters as `⇧` does, in the text's
      colour, at every view scale; the search's hint strip says `⌘s` stop and
      `⌥Enter` go there; a file dialog's greeting says "⌘-click selects";
      `delightfile --help` in Terminal says `⌘Enter`.
- [ ] A `keymap.toml` with `"super+k" = "quit"` under `[files]` (M2.21): Cmd+K
      quits, and the config warning says "super is Cmd, which is Ctrl on macOS".
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
- [ ] With nothing trashed from delightfile (M2.9): `g t`
      shows "empty" and under it "Only files trashed from delightfile are listed —
      Finder's Trash may hold more"; "Empty trash" toasts "Destroyed N items — only
      what delightfile trashed; Finder's Trash may hold more" and leaves Finder's own
      items in the Trash.
- [ ] Tags (`T`): a tag set in the app shows in Finder, a colour tag in its colour;
      a tag set in Finder shows on the row; a copy (same volume and across) keeps
      them; `T` on a symlink is refused (M2.33).
- [ ] Owner linemode (`m o`): the logged-in user and `staff` by name (M2.4).
- [ ] Emptying old trash (M2.35): with `[mgr] trash_keep_days = 1`, trash a file
      from delightfile, quit, and change its line's first field in
      `~/.local/state/delightfile/trash-journal` to two days ago; the next window
      takes it out of Finder's Trash and toasts what it removed, and Finder's own
      items stay where they are; a second window the same day does not purge
      again (`trash-journal.purge` beside the journal holds the day).
- [ ] Permissions (`C`) on macOS (M2.28): toggle a bit on a file and Apply, and
      `ls -l` agrees; `u` puts it back; a folder shut to its owner (`chmod 000`)
      asks to Apply again rather than failing.
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
- [ ] Drag-out, closer (M2.12): the drag leaves the window without a gap — the ghost
      goes at the edge and the system's drag picture takes over under the pointer,
      right way up, in the ghost's colours, with no dark halo; two files dragged
      onto the Desktop make two copies and no text clipping; the same drag onto a
      TextEdit document types the two paths; a drag that is let go over nothing
      springs back and the window takes keys again at once; after any drag out,
      the next click in the window is an ordinary click (AppKit keeps the
      `mouseUp` that ended the drag, and winit may not see the button come up).
      If the drag never starts, winit's `CursorMoved` is not inside the
      `mouseDragged` (02-macos.md Open questions).
- [ ] Escape mid-drag cancels.
- [ ] A drop from Finder (M2.11): the folder row under the pointer lights as the
      drag passes over it and takes the drop; dropped on a file row, it lands in the
      folder on screen.
- [ ] `Y` on a PNG, then File → New from Clipboard in Preview (M2.10): the picture;
      `Y` on a text file, then Cmd+V in TextEdit: its text; a screenshot copied with
      Cmd+Ctrl+Shift+4, then `p`: a `clipboard_….png` in the folder.
- [ ] `p` right after copying something new in another app (M2.13): what was just
      copied is pasted, not what was on the pasteboard before.

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
- [ ] The macOS openers (M2.17): Enter on a picture, a video, a PDF, a song, a
      `.stl`, a `.ttf`, a `.gcode` and an unknown file opens each in its default
      app (`open`); `O` on a picture offers Preview (`edit-image`) after it, on a
      video mpv (`play`) when installed; on a text file Enter opens Zed when
      installed, and `O` offers Zed, `edit`, `open` and a Terminal in the
      folder; `edit` runs `$EDITOR` in ghostty when installed, else in
      Terminal.app; `terminal-here` on a folder opens Terminal.app there, or the
      app `TERMINAL_APP` names; nothing says `xdg-open` or `setsid`.
- [ ] The `g` bookmarks (D5.11): `g h`, `g c`, `g d`, `g w`, `g D` (Shift+d)
      and `g o` go to home, `~/.config`, Downloads, `~/Work`, the Desktop and
      Documents, and the which-key card lists those six and no server mounts.
- [ ] `M` volumes card: internal disk, a USB stick, a mounted dmg, a network share;
      eject the USB stick from the card and confirm Finder agrees.
- [ ] The volumes card, closer (M2.14): the boot volume reads "Macintosh HD", its
      size and "APFS", and none of the system's hidden volumes is listed; `u` on a
      mounted dmg unmounts it; `e` on the boot volume says it cannot be ejected; `m`
      on any disk says macOS mounts disks itself; a share connected in Finder is in
      the Network section and `u` there unmounts it.
- [ ] Connect (M2.15): `c` then `smb://<server>/<share>` brings up Finder's own
      password dialog, and once it is answered the window goes into the share
      under `/Volumes`; `sftp://host` is refused with "use the sftp: bookmark
      instead" and `davs://…` with where Finder mounts WebDAV; a dialog left
      unanswered for more than ten seconds toasts "Finder was asked to connect to
      …" (not "Connected to"), and once it is answered the share is in `M`'s
      Network section.
- [ ] Openers run detached (M2.16): open a file in TextEdit with `o`, quit
      delightfile, and TextEdit stays; quit TextEdit while delightfile runs and
      `ps -o stat= -p <its pid>` shows nothing, not a `Z`.
- [ ] SFTP: `Connect to:` an ssh host that is in `~/.ssh/config`; list, preview a
      text file, copy a file down, copy a file up.
- [ ] A cloud remote open, then `kill -9` the app: `rclone rcd` is gone within a
      second (`ps aux | grep rclone`) (M2.29).
- [ ] `alt+p` sync to a server with Apple's `rsync` only: refused with "Sync to a
      server needs rsync 3.1 or newer — `brew install rsync`", said once
      (`RSYNC_HINT`); with `brew install rsync`: it runs (M2.6).
- [ ] `--chooser-file` and the portal flag are absent from `--help`.
- [ ] Zoxide jumps if `zoxide` is installed via Homebrew.
- [~] Open With from Finder: right-click a folder, Open With, delightfile opens
      a window on that folder. — skipped: not available. The bundle declares
      no document types until `02-macos.md` M2.36 hands the app the folder
      Finder chose, so delightfile is not in Finder's list; check that it is
      not.

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
- [ ] On a screen smaller than the window's opening size (a VM at 1024×768),
      the window opens inside the work area, frame and all, above the taskbar
      (df-app's window fit, 04-windows.md Decisions log).
- [ ] `[flavor] mode = "auto"` (W4.33): the first frame is on the side
      Settings → Personalisation → Colours is on; switching it there turns the
      window at once.
- [ ] One header (W4.39): no "delightfile" caption above the window; the
      top row (☰, breadcrumb, counter) is the top of the window, with
      minimize, maximize and close at its right end, drawn by the window in
      Windows 11's shapes, and the counter a gap short of them. The window keeps its rounded corners, its
      shadow and its resize edges, the top one included.
- [ ] The band is the title bar (W4.39): a drag from its empty space (between
      the crumbs and the counter, beside or under the buttons) moves the
      window; a double click there maximizes, and again restores; a right
      click opens the system menu; ☰, a crumb and the counter still answer
      as themselves.
- [ ] The caption buttons (W4.39): each works, on release, and a press
      dragged off its button does nothing; hovering Maximize brings up Snap
      Layouts; under the pointer minimize and maximize lift on a plate and
      close turns red with a white ×, each a shade stronger held; maximized,
      the middle glyph is restore's two squares; the glyphs turn with View ▸
      Appearance.
- [ ] Two tabs (W4.39): with `t` or the menu's new tab, the strip is the top
      of the window and the top row under it; the tabs, their `×` and the `+`
      answer; the space after the `+` drags the window; the strip stops short
      of the buttons.
- [ ] Maximized (W4.39): nothing is cut off at the top, no strip of the
      window hangs off the screen, and the band's controls answer; restored,
      the window is where it was.

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
- [ ] Watch (W4.4): a burst of a thousand files landing (an unzip in PowerShell)
      is one refresh, not a thousand; the folder on screen deleted from Explorer
      makes the pane leave it; a folder above the one on screen can still be
      renamed and deleted from Explorer while delightfile shows it (the watch's
      handles share delete).
- [ ] Size column, `m u` usage, git chip, hidden files (attribute-hidden and
      dot-prefixed both count as hidden per Phase 3's decision).
- [ ] Symlink creation: refused with the privilege toast when not elevated, works
      with Developer Mode on; junctions display as links.
- [ ] A file on `C:` (W4.29): Tab shows no Owner or Permissions row, no octal
      in the title and no permission-bit keys in the hint; the Permissions
      column says `rw` or `ro` (and `h` when hidden) and the Owner column `—`;
      `C` toasts "Permissions can't be changed on this platform" and the menu's
      Permissions… is greyed. A file on an SFTP server keeps all of it.
- [ ] A name Windows will not make (W4.12): `Save as:` and the bulk card
      refuse `a:b`, `con` and `x.` in their own words, naming the character.

### 5.4 Operations
- [ ] Copy, move, within and across drives, conflict card, cancel.
- [ ] Trash with `d`: item in the Recycle Bin; `u` after trash does what Phase 4
      decided (restore via the journal or a toast pointing at the Recycle Bin); `D`
      permanent; the trash view tab does what Phase 4 decided.
- [ ] `d` (W4.7): an item on a USB stick or a network share is refused for the
      Recycle Bin and goes to the permanent-delete confirm, never deleted without
      a word; a file larger than the bin's size limit on a fixed drive brings
      up the shell's own "permanently delete?" dialog, whose No leaves the file
      and toasts that the move was cancelled (04-windows.md Decisions log,
      W4.7).
- [ ] No console window flashes (W4.2): while the git chip refreshes, while a
      7z or a `.tar.gz` is listed or extracted, while `A` writes an archive.
- [ ] Rename with a case-only change; bulk rename.
- [ ] Extract zip, tar.gz; 7z when `7z.exe` is on `PATH`.

### 5.5 Clipboard and drag
- [ ] `Y` then Ctrl+V in Explorer pastes a copy (`CF_HDROP` with `Preferred
      DropEffect` copy; `y` and `x` are the window's own clipboard, as on
      Linux, so no cut reaches Explorer); Ctrl+C in Explorer then `p`; `c c`
      then Ctrl+V into a text field pastes the path; `Y` on a PNG then Ctrl+V
      in Paint pastes the picture; a Snipping Tool capture then `p` saves a
      `clipboard_….png`.
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
- [ ] SFTP through the built-in OpenSSH client (W4.20, W4.21): no console window
      appears; a listing streams and a large download and upload complete; a
      key given as `-i ~\.ssh\key` in `vfs.toml` works; a host that refuses
      says ssh's own words; pulling the network cable mid-transfer ends in a
      timeout toast, not a hang.
- [ ] Where things live (D5.1): `%APPDATA%\delightfile\delightfile.toml` is read;
      `%LOCALAPPDATA%\delightfile\state` survives a restart; the services of
      yazi's `%APPDATA%\yazi\config\vfs.toml` are listed; `Z` finds zoxide's
      `%LOCALAPPDATA%\zoxide\db.zo`.
- [ ] Connect (W4.19): `c` then `smb://<server>/<share>` brings up Explorer's
      password dialog, and once it is answered the window goes into
      `\\<server>\<share>`; a dialog left unanswered for more than ten
      seconds toasts "Explorer was asked to connect to …"; `u` on a mapped
      letter in `M` puts the mapping away; `m` and `u` on a disk say what
      Windows does instead.
- [ ] A typed line (W4.3): `;notepad` opens Notepad with no console window
      beside it; in a share's folder `:cd > here.txt` writes `here.txt` there,
      and `M` lists no letter the line left mapped.
- [ ] The `g` bookmarks (D5.11): `g h`, `g c`, `g d`, `g w`, `g D` and `g o` go to
      the user profile, `%APPDATA%`, Downloads, `Work`, the Desktop and
      Documents; the which-key card lists those six and no server mounts.

### 5.8 Runs

Not release passes (§6): each is a CI debug build of a branch, launched by
double-click from the host share, and the lines above stay unticked until the
release zip is run.

- 2026-09-29, df-app agent (`port/windows-app`) — Windows 11 in the dockur VM
  (QEMU, no GPU passthrough, 1024×768), driven through its noVNC viewer by
  mouse and plain keys; the viewer passes no Shift or modifier chords, so no
  chord was tried. Screenshots in the agent's scratchpad (`winapp/shots/`),
  named by the pre-rebase commit each build was.
  - Run 36653756431 (the build of W4.25, 3b368bd, with W4.1, the window fit
    and W4.23 under it): the window opened inside the screen, frame and all,
    above the taskbar, with no console beside it (`01-…`); it drew on the
    software adapter; ↓ and ↑ moved the cursor and wrapped, `/` opened find
    and took typing, and `j`, `k`, `h` moved nothing, as on Linux.
  - Run 36657284865 (W4.26, e28af95): Ctrl+C on a file in Explorer, then `p`
    in another folder of the share, pasted the file (`02-…`); `c c` then
    Ctrl+V into Explorer's search box pasted `\\host.lan\Data\…` (`03-…`);
    `M` listed "Windows (C:)" with its size, NTFS and `C:\`, and "nothing
    mounted" under Network (`04-…`); Enter on it went to `C:\` (`05-…`). The
    card's Places still listed Linux's bookmarks, which main's §6 Windows
    table (D5.11) has since replaced (seen in the last run).
  - Run 36658528615 (W4.15, 65c8b50, with W4.29 and W4.33 under it): Tab on a
    file had no Owner or Permissions row (`06-…`), but its title still showed
    `0644` and its hint the permission-bit keys; the menu's Permissions… was
    greyed (`07-…`); `Go to:` took `C:/Users`, and `..\from` from a folder of
    the share (`08-…`); `;notepad` opened Notepad and a `cmd` console beside
    it that stayed while Notepad ran.
  - Run 36662477949 (4d56ecf, the branch's code as handed back): Tab on a
    file in the share has no octal in its title and "Space hash" alone in its
    hint (`09-…`); `;notepad` there opened the file in Notepad with no console
    (`10-…`); while Notepad ran the Places card listed "Z: → \\host.lan\Data",
    the letter `pushd` mapped (`11-…`), and once Notepad was closed `r` found
    "nothing mounted" (`12-…`). The card's Places are §6's Windows six, `g D`
    and `g o` among them.
  - Incident: the viewer's `semicolon` key did not arrive as `;`, so the
    `explorer .` typed after it ran as keys in `C:\` — a cut chip, the hidden
    files toggle and a step into `inetpub`. The Tasks panel showed nothing
    had run but the earlier paste inside the share, the cut was never
    pasted, and nothing was changed; `;` has been typed with the viewer's
    text entry since. One screenshot timed out in the viewer; the next call
    worked.
  - Not seen: everything else in §5, among it scaling, snapping, a second
    window, AltGr and the Windows key (no chords), long paths, reserved
    names, watching, links, a copy or move inside the window, trash
    (refused until W4.7), extracting, drag in or out, media preview and
    sound, the openers (Linux's table until D5.4), eject, connect, SFTP,
    going to a bookmark, and `auto`.
- 2026-09-30, titlebar agent (`port/titlebar`, W4.39) — the same VM. Before:
  the 2026-09-29 run's first screenshot (d8919ef) has both headers, the
  system's "delightfile" caption with its three buttons over the app's own
  top row (kept as `titlebar/shots/00-before-d8919ef-two-headers.jpg` in the
  agent's scratchpad).
  - Run 36727934410 (04a3425) built and was put in the share as
    `delightfile\titlebar-04a3425\`, but not run: the VM was at its lock
    screen, and past it at the sign-in for the account `admin`
    (`titlebar/shots/01-vm-locked-sign-in.jpg`), which an agent may not
    pass. Escape went back to the lock screen; nothing else was touched.
  - Run 36731513722 (f2cd5c8, the caption buttons drawn by the window,
    Brian's call that day) was put in the share as
    `delightfile\titlebar-f2cd5c8\`, and not run either: the VM was still
    at its lock screen (`titlebar/shots/02-vm-still-locked-f2cd5c8.jpg`),
    and nothing was pressed.
  - The same build, once the coordinator had restarted the VM (it signs in
    by itself on boot), opened from the share after the Run prompt: **one
    header** (`titlebar/shots/10-f2cd5c8-single-header.jpg`, enlarged in
    `10-crop-left.png` and `10-crop-buttons.png`). No "delightfile" caption
    is left over the window; its top row — ☰, `\\host.lan\Data ›
    delightfile › titlebar-f2cd5c8`, `1 / 8` — is the top of the window, on
    the window's own corners and shadow, and the three buttons the window
    draws, minimize, maximize and close in Windows 11's shapes, sit at its
    right end with the counter's row a gap short of them. It drew on the
    software adapter as before.
  - Then the viewer failed: after the pointer was put over Maximize for
    Snap Layouts, two captures in a row came back 310 × 45 pixels
    (`11-…`, `12-…`) and the next timed out, so the run stopped there, by
    the rule for a viewer that misbehaves twice running. The window was
    left open on the VM's desktop; nothing else was pressed.
  - Not seen: the other W4.39 lines in §5.1 — a drag, a double click and a
    right click on the band, Snap Layouts, the three buttons' hover, press
    and commands, ☰, two tabs in the band, a maximized window and the
    theme switch.

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
- (df-app) 2026-09-29 — §4 gained the on-screen checks for Phase 2's df-app
  tasks (M2.9–M2.16, M2.19, M2.20, M2.23–M2.26, M2.30), each marked with its
  task; the appearance line no longer says the app does not follow the system,
  since `auto` now does (M2.30).
- (macos-finish) 2026-09-29 — §4 gained the on-screen checks for the tasks
  that needed both branches (M2.17, M2.21, M2.28 through the app, M2.35, and
  05's D5.5 and D5.11); the connect line says what a slow dialog toasts now
  that Brian settled it (M2.15), and the M2.9 line no longer waits on M2.8.
- (macos-finish) 2026-09-29 — After Brian's calls: the openers line checks
  the default app for every kind Linux gives delightviewer, the bookmarks
  lines (§4.7 and §5.7) have `g D` and `g o`, and the rsync line quotes the
  sentence that now reads once.
- (df-app) 2026-09-29 — §5 gained the on-screen checks for Phase 4's df-app
  tasks (the window fit, W4.3, W4.12, W4.19, W4.29, W4.33), and §5.5's
  clipboard line says what `y`, `x` and `Y` do on Windows (04-windows.md,
  W4.16's log entry). §5.8 records the Windows 11 VM runs, which were not a
  release pass: CI debug builds, a viewer that passes no modifier chords.
- (titlebar) 2026-09-30 — §5.1 gained W4.39's checks: one header, the band
  as title bar, the caption buttons with Snap Layouts and their light and
  dark, the strip in the band with two tabs, and a maximized window; §5.8
  records that the VM was locked when the build was ready. The buttons'
  line was rewritten the same day for the buttons the window draws itself
  (04-windows.md, Decisions log).

## Open questions

- Which machine Brian will use for the macOS pass (§3).
