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
- [ ] One header (M2.37): no "delightfile" title and no grey title bar over
      the window; the top row is the top of the window, the traffic lights
      over its left end on the window's ground and the breadcrumb starting a
      gap past the zoom button; there is no ☰ button, the first crumb sitting
      in the row's corner where it was. The lights work: close, minimize,
      zoom, and Option-click zoom.
- [ ] Lights centred on the row (M2.37): the traffic lights' middle is level
      with the top row's middle, the breadcrumb's text beside them; with two
      tabs, level with the strip's; still there after a resize from each edge,
      after minimizing and restoring, after a rename prompt shows an error on
      a second line, and after the window loses and regains the keyboard;
      in full screen AppKit's own title bar comes down with the lights where
      it always has them, and back out of full screen they are level with
      the row again. Each light answers its hover and its click over the
      whole of it.
- [ ] The band is the title bar (M2.37): a drag from the row's empty space
      (between the crumbs and the counter, beside the lights, the margin
      above the row) moves the window; a double click there zooms, and again
      unzooms; with System Settings → Desktop & Dock → "Double-click a
      window's title bar to" Minimize it minimizes, and with Do Nothing it
      does nothing. A crumb, the counter and the chips still answer as
      themselves, and a drag that starts on a crumb or a tab does what it
      does on Linux and does not move the window (02-macos.md, Open
      questions).
- [ ] Two tabs (M2.37): with `t`, the strip is the top of the window, past
      the lights, and the top row under it runs the window's width; the tabs,
      their `×` and the `+` answer; the space after the `+` drags the window.
- [ ] Full screen with the band (M2.37): the row is at the window's top-left
      with no gap for the lights, and the title bar that comes down under the
      pointer is AppKit's; back out of full screen, the row is past the
      lights again.
- [ ] The menu bar (M2.37): delightfile, File, Edit, View, Go, Window, Help.
      delightfile has About delightfile (the About panel), Services, Hide
      delightfile ⌘H, Hide Others ⌥⌘H, Show All, and Quit delightfile ⌘Q,
      which quits as Cmd+Q does (the cwd-file line above holds for it too).
      File, Edit, View, Go and Help hold the ☰ menu's rows as 02-macos.md's
      Decisions log lays them out, each once; Window has Minimize ⌘M, Zoom,
      Bring All to Front and the window below them, and no Show Tab Bar or
      Merge All Windows anywhere.
- [ ] The menu bar's rows (M2.37): choosing one does what the ☰ row did on
      Linux (View ▸ Show hidden files, a place under Go, Edit ▸ Clipboard,
      Help ▸ Keyboard shortcuts); View's ticks follow `-`/`=` and `.`, and
      Sort's the sort keys; Go ▸ Go to path… is grey inside an archive; with
      a rename prompt, the help sheet or the palette up, every row of File,
      Edit, View, Go and Help is grey; `F10` still opens the app menu in the
      window, from the row's left corner.
- [ ] Keys in the menu bar (M2.37): rows show their keys the Mac way — New
      window ⌘N, New tab T, Redo ⇧U, Command palette… ⌘P, Keyboard shortcuts
      F1 — and no row with a two-key sequence shows one. Pressing a shown key
      does what the keymap says, not the menu: `y` typed into a rename prompt
      types `y`, ⌘V in a rename prompt pastes text, and ⌘V in the list pastes
      files; the menu's title does not flash for them. ⌘M minimizes, also
      with the pointer over a playing clip.

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
- [ ] Left alone (W4.47): after the last key a toast rises, rests and goes
      by itself, a copy's bar in the tasks panel (`w`) moves to done, and a
      file made by another program appears in the listing, all with nothing
      touched; with `DF_FRAME_LOG=1` (the log through PowerShell's
      `Start-Process -RedirectStandardError`) the settled window logs no
      frame. Judge a 2.2 s notice by eye or by the log, not by a screenshot
      taken seconds later; an undo toast (8 s) is easier to catch.

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
- [ ] One separator (W4.46): after `g D` the breadcrumb, `Go to:`'s seed and a
      pin made there say `C:\Users\<name>\Desktop`, never `…\<name>/Desktop`;
      `Go to:` with `C:/Users/<name>/Documents`, `//host.lan/Data` and a path
      typed with both separators goes there and shows it with `\` alone.
- [ ] Icons without a Nerd Font (W4.42): no `/` before a folder's name; the
      list, the grid at the top view step and the search panel's hits draw a
      folder with a tab in its colour, a page in its kind's, a link's arrow
      and a program's `▶`, crisp at 100 % and 125 %, the names where they
      were; the Places card's drives, shares and "Connect to server…" have
      their pictures, and an archive's preview header its mark.
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
- [ ] "Empty trash" (W4.8): the palette's "Empty trash" row, there on Windows
      in any folder, opens "Empty the Recycle Bin? N items · X will be deleted
      for good." with the bin's own count and no names under it; Empty empties
      it and toasts "Emptied the Recycle Bin"; with the bin already empty, no
      card and "The Recycle Bin is already empty".
- [ ] No console window flashes (W4.2, W4.38): while the git chip refreshes,
      while a 7z or a `.tar.gz` is listed in the preview or extracted, while
      `A` writes an archive, while the search panel runs fd or rg.
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
- [ ] An SVG (W4.44): a drawing with a gradient, a path and words previews
      as its picture, fitted to the pane, the words in a system face; not
      "no decoder".
- [ ] An SVG or a PNG with clear parts (W4.48): the pane's ground shows
      through them on its first visit and on every visit after, in a later
      run too, with no black square under the picture.
- [ ] An Affinity file (W4.45): an `.afdesign` or `.af` with its thumbnail
      previews as that picture with "embedded preview" in the corner; one
      without is a card with its name, "Affinity … document", its size and
      its dates, not a hexdump. (Linux too.)
- [ ] Audio output through the default device; changing the default device
      mid-playback does not crash.

### 5.7 Integration
- [ ] Enter opens with the file association; `o` shows the Windows opener defaults;
      `terminal-at` opens Windows Terminal (or `cmd` when `wt` is absent) in the
      folder.
- [ ] Enter is a double-click (W4.41): Enter on a picture, a text file, a
      PDF, a zip and a file with no association does what Explorer's
      double-click does (the last, "How do you want to open this?"); `O` on
      each ends in "Open with…", which opens Windows' chooser for that file,
      a name with spaces included; `O` lists only programs this machine has
      (no Zed, no Chrome, no VS Code row where VS Code is not installed);
      `O` on a folder offers Open in Explorer and Terminal here.
- [ ] A font (W4.43): Enter on a `.ttf` installs it for this user, toasting
      "Installed <family>", and the face is in Settings → Fonts and in
      `%LOCALAPPDATA%\Microsoft\Windows\Fonts`; `O` offers "Preview in
      Windows Font Viewer", which opens it there.
- [ ] `M` drives card lists every drive with type and free space; eject a USB stick
      if Phase 4 implemented it.
- [ ] SFTP through the built-in OpenSSH client (W4.20, W4.21): no console window
      appears; a listing streams and a large download and upload complete; a
      key given as `-i ~\.ssh\key` in `vfs.toml` works; a host that refuses
      says ssh's own words; pulling the network cable mid-transfer ends in a
      timeout toast, not a hang.
- [ ] Cloud remotes (W4.32), with rclone installed and a remote in
      `%APPDATA%\rclone\rclone.conf`: the remote lists, a download and an
      upload complete, and no console window appears; while it is open
      `%LOCALAPPDATA%\delightfile\run` holds one `rclone-<pid>-<name>-<n>.sock`,
      and after quitting it holds none.
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
- [ ] The `g` bookmarks (W4.40, which replaced D5.11's six): `g h`, `g d`,
      `g D`, `g o`, `g p`, `g v`, `g m`, `g c` and `g a` go to the profile,
      Downloads, Desktop, Documents, Pictures, Videos, Music (where the shell
      says each is, OneDrive's included), `C:\` and `%APPDATA%`; the which-key
      card lists them as "Home · C:\Users\<name>" and so on, no `~`, no `g w`
      and no server; the help sheet lists the same; `g /` opens the Places
      card, whose Places say Home and AppData by name with their paths.

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
- 2026-09-30, the finish agent (`port/windows-finish`) — not run. At the
  first look (09:22 on the VM's clock) the VM was at the sign-in for
  `admin`, asking for a password (`shots/01-locked-sign-in.jpg` in the
  agent's scratchpad), which no agent enters. At the second (10:27, after
  the coordinator's restart) it was signed in, the titlebar agent's window
  of f2cd5c8 still open; a right click and then a left click on the
  taskbar's File Explorer button changed nothing on screen, and the next
  capture timed out, so the viewer was left alone, by the rule for one that
  misbehaves twice running. Nothing in any window was pressed. The build of
  the branch at a9d6d4f (run 36737465177) is in
  `\\host.lan\Data\delightfile\a9d6d4f\` (ac1f843's, from before the
  rebase onto `main` at 4c0012c, beside it), with `scratch\` in it
  (`pack.7z`, `pack.tar.bz2`, `trash-me.txt`, `recycle-me.txt`), for the
  checks this round was to make: "Empty trash"
  from the palette, its card and what emptying says; `d` on a copy of
  `recycle-me.txt` pasted onto the Desktop, then `u`'s words; `pack.7z` and
  `pack.tar.bz2` listed in the preview with no console window, and whether
  7-Zip is on the VM at all; `;notepad` in the share as before.
- 2026-10-01, the polish agent (`port/windows-polish`, W4.40–W4.46) — the
  same VM, which was at its lock screen (`winpolish/shots/00-lock-screen.jpg`
  in the agent's scratchpad) until the container was restarted; it came
  back signed in. Builds put in the share as `delightfile\polish-<sha>\`,
  sample files in `delightfile\polish-scratch\` (an SVG with a gradient, a
  path and words; `Sample Design.afdesign`, a synthetic Affinity file whose
  header points at a 512×300 thumbnail with a larger 900×600 picture placed
  before it; `No Thumbnail.af`, the signature and noise; `Hack-Regular.ttf`,
  egui's face; `notes with space.txt`; `a folder`). Screenshots in
  `winpolish/shots/`. Hack Regular is installed for `admin` now
  (`%LOCALAPPDATA%\Microsoft\Windows\Fonts\Hack-Regular.ttf`), by W4.43's
  check; nothing else on the VM was changed.
  - Run 36905030633 (e02d7c7), opened from the share with no Run prompt:
    - W4.42: no `/` anywhere; the parent column's folders are drawn
      folders in blue, the DLLs pages with `▶` in green, an SVG and the
      Affinity files pages in mauve, the font's in sky (`01-…`).
    - W4.40: `g` brings up the which-key card with `h Home ·
      C:\Users\admin`, `d Downloads · C:\Users\admin\Downloads`, `D`, `o`,
      `p`, `v`, `m` the same, `c Drive C:\`, `a AppData ·
      C:\Users\admin\AppData\Roaming`, `/ Places: drives and network`, and
      no `w` and no server (`02-…`); clicking its `D` row went to the
      Desktop, the breadcrumb `C: › Users › admin › Desktop` (`03-…`);
      `g a` went to `C:\Users\admin\AppData\Roaming`.
    - W4.46: `Go to:` opened from the last crumb there seeded
      `C:\Users\admin\Desktop`, every separator `\` (`04-…`); typed
      `//host.lan/data/delightfile/polish-scratch` it went to
      `\\host.lan\data › delightfile › polish-scratch` (`05-…`).
    - W4.44: `logo.svg` previewed as its picture — the gradient, the
      triangle and "delightfile" in Segoe UI — not "no decoder" (`06-…`).
    - W4.45: `Sample Design.afdesign` previewed as its thumbnail (the
      gradient and the white disc, not the larger green picture placed
      before it) with "embedded preview" in the corner (`09-…`); `No
      Thumbnail.af` as its card: the name, "Affinity document", 9.0 KB,
      "Modified 2026-10-01 13:14", "Created 2026-10-01 13:14" (`08-…`).
    - W4.43: Enter on `Hack-Regular.ttf`, and the menu's Open with ▸
      install-font, each made a task "Install Hack Regular" that ended
      done (`13-…`), and the font was not in
      `C:\Users\admin\AppData\Local\Microsoft\Windows\Fonts` (`14-…`);
      Explorer's menu on the same file offers Install (`17-…`).
    - The row menu's Open with ▸ lists the openers by their ids
      (`install-font`, `font-viewer`, `open-with`; `11-…`), as on Linux
      (`zed`, `edit`); the `O` picker has the descriptions (04 Open
      questions).
    - No toast was seen: not after the installs, not after `u` with nothing
      to undo, `g r` outside a repository or `c c` (`15-…`, `16-…`).
    - Incident: OneDrive's "Turn On Windows Backup" notification came up
      over the window (`07-…`); it was closed with its ×, nothing chosen.
  - Run 36911202413 (0c93a72, the verb's apartment kept and pumped):
    - `u` with nothing to undo toasted "Nothing to undo" while the pointer
      moved over the window (`18-…`): a toast is drawn in a frame input
      brings, and was missed before by waiting with the window left alone
      (04 Open questions).
    - W4.43: Enter on the font, task done, still no Hack Regular in the
      per-user Fonts folder (`19-…`, `20-…`).
    - W4.40: `g /` opened the Places card: Windows (C:) under Devices,
      "nothing mounted" and "Connect to server…" under Network, and Home,
      Downloads, Desktop, Documents, Pictures, Videos, Music, C: and AppData
      under Places, each by name with its path and its chord (`21-…`).
    - W4.41: Enter on `notes with space.txt` opened it in Notepad (`24-…`);
      its Open with ▸ offered open, notepad and open-with — this VM has no
      VS Code or Notepad++ — (`22-…`), and a folder's explorer and
      terminal-here (`25-…`). open-with, then `rundll32
      shell32.dll,OpenAs_RunDLL`, put up no chooser (`23-…`).
  - Run 36914607187 (6478f43, the verb found through the item's menu and
    allowed its dialogs; Open with… the `openas` verb):
    - W4.43: Enter on `Hack-Regular.ttf` toasted "Installed Hack Regular"
      (`27-…`), and `Hack-Regular.ttf` is in
      `C:\Users\admin\AppData\Local\Microsoft\Windows\Fonts`, 32 files
      where there were 31 (`28-…`). No dialog came up.
    - W4.41: open-with on `notes with space.txt` still put up no chooser,
      nothing behind the window either (`26-…`); Explorer's own Open with ▸
      Choose another app on the same file put up "Select an app to open
      this .txt file", Notepad the default, with Always and Just once
      (`29-…`), closed with Esc.
  - Run 36917221800 (b2989b8, Open with… `SHOpenWithDialog`):
    - W4.41: open-with on `notes with space.txt` put up "Select an app to
      open this .txt file" over the window, Notepad the default, Windows
      Media Player Legacy under More options, and Just once with no Always
      (`30-…`); Esc closed it, no toast, the window answering the pointer
      at once (`31-…`).
- 2026-10-01, the repaint agent (`port/windows-repaint`, W4.47 and the
  SVG's black, 04-windows.md §14) — the same VM, signed in; no restart.
  Builds in the share as `delightfile\repaint-<sha>\`, each with
  `samples\` (`disc.svg`, a blue disc on nothing; `logo.svg`; `a folder`;
  a 1 GiB `big.bin` of zeros) and launchers that set `DF_FRAME_LOG=1` and
  start the program through PowerShell's `Start-Process
  -RedirectStandardError`, the log in `logs\` (cmd's `2>` does not reach
  a windows-subsystem program: its log went to the console). Screenshots
  and logs in `repaint/shots/` and `repaint/logs/` in the agent's
  scratchpad. Nothing on the VM was changed outside those folders.
  - Run 36921679025 (6fd5c2a, `main`), log in the console: `u` with
    nothing to undo, then screenshots seconds later with no toast
    (`01-…`, `02-…`); the console's lines were the end of a fade, the toast
    long gone (`04-…`). `j` did not move the cursor, as on 2026-09-29
    (`05-…`); a hovered row lit at once (`06-…`).
  - Run 36926544921 (d62bd26, 6fd5c2a with a frame-log probe, not kept),
    with its present mode and loop as `main`'s (`DF_REST=old`):
    - `u` from idle: the key's frame took 23 ms, the next was asked for on
      the way into a `Wait` and came 4 ms later; the rise ran, a 1.65 s
      deadline woke the loop as `ResumeTimeReached`, and the fade ran, with
      no input after the key (`logs/frames-old.log`, 21:30:18–21:30:20).
    - A file made from the host (`zz-from-linux.txt`) appeared in the
      listing with no input at all, its frame the watcher's (`16-…`).
    - `a`, `zz-made.txt`, Enter: "Created file zz-made.txt · u undo" was on
      screen after the key (`17-…`) and gone by itself 8 s on (`18-…`); in
      the log the rise came on deadlines after the key and the fade on one
      5.5 s out.
    - `y` on `big.bin`, Enter on `a folder`, `p`, `w`, hands off: the
      task's bar at 177 MB of 1.0 GB (`19-…`), at 493 MB (`20-…`), done
      with "Copied 1 item · u undo" up (`21-…`), and the toast gone by
      itself (`22-…`); no input after the `w` in the log, the frames the
      task's and the deadlines'. From the toast's end the window drew
      nothing for the 23 s until it was closed.
    - Earlier the same session, a screenshot right after `u` had no toast
      with Mailbox (`08-…`), and had it with the same probe under
      `DF_PRESENT=fifo` (`12-…`) and with run 36930234032 (6efe128, Fifo on
      Windows, not kept; `23-…`); a screenshot a second after `p` had none
      with either (`10-…`, `24-…`). The tool's actions come seconds apart
      (a key and the next pointer move 5.1 s apart in the log) and a notice
      lives 2.2 s, so whether one is caught is chance; the states above,
      which last, settle it. W4.47 is the viewer, not the window.
    - `disc.svg`: clear around the disc on its first visit (`14-…`) and on
      a second visit in the same run; in the next run, over a black square
      (`15-…`), the yazi-cache JPEG the first visit wrote (04-windows.md,
      Decisions log; fixed as W4.48).
  - Run 36935273351 (ab95ef4, W4.48; the same change, its message
    amended): a fresh `disc.svg` clear around the disc on its first run
    (`25-…`) and on a second run, the program closed and started again
    (`26-…`), with no JPEG left for it to find; and the `disc.svg` an
    earlier build had cached a black JPEG for, opened from the same build,
    clear once the picture landed (`27-…`).

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
- (mac-native) 2026-09-30 — §4.1 gained M2.37's checks: one header with the
  traffic lights over the top row and no ☰, the band as title bar with the
  double-click setting, two tabs, full screen, the menu bar's seven menus,
  its rows and their greys, and keys shown in the menus but answered by the
  keymap. Nothing of it has been seen on a Mac.
- (mac-native) 2026-09-30 — §4.1 gained "Lights centred on the row" once
  Brian had the lights moved to the row's middle (02-macos.md, Decisions
  log), and the one-header line no longer asks where they sit.

- (finish) 2026-09-30 — §5.4 gained "Empty trash" (W4.8) and the console
  line names the preview's listings and the search panel (W4.38); §5.7
  gained cloud remotes (W4.32); §5.4's line on a file too big for the bin
  says what the shell now asks (W4.7's `FOF_WANTNUKEWARNING`). §5.8 records
  that this round's VM check was not made: a sign-in screen, then a viewer
  that stopped answering.

- (repaint) 2026-10-01 — §5.1 gained the window left alone (W4.47), with
  the warning that a screenshot through the viewer comes seconds after a
  key, longer than a notice lives; §5.6 gained a picture with clear parts
  (W4.48); §5.8 records the round, and how to get the frame log out of a
  windows-subsystem program on the VM.

## Open questions

- Which machine Brian will use for the macOS pass (§3).
