# 02 — macOS

Status: **not started**

Scope: native macOS bodies behind the Phase 1 seam, so that the `.app` produced by
`06-build-and-release.md` is a working file manager on Apple Silicon: file watching,
clone-copy, trash, clipboard, drag in and out, volumes, openers, fonts, keyboard,
scheduling priority. Everything here is "done" at *compiles, unit tests pass on the
macOS runner* and is then listed in `07-verification.md` §4 for a human pass. No
task here changes Linux.

Prerequisites: `01-platform-seam.md` complete (df-app compiles on
`aarch64-apple-darwin`); `06-build-and-release.md` B6.3, B6.7–B6.9 (the runner
builds df-app). Phase 3 is **not** a prerequisite except where a task says
"(needs P3.n)".

Factual basis: `appendix-inventory-df-core.md` §1–§2 (macOS-differs and Linux-only
rows), `appendix-inventory-df-app.md` §1–§3.

## Decisions already made

- **kqueue, not FSEvents, for the watcher.** `WatchEvent` is already coarse
  (`Changed(dir)`, `Gone(dir)`, `Overflow`, `fs/watch.rs:56–66`), the watched set is
  the handful of directories on screen, and every kqueue symbol is in `libc` with no
  new FFI. FSEvents would need CoreServices bindings and a run loop for a precision
  the model does not use. One fd per watched directory is fine at this scale.
- **`fclonefileat` for reflink copies**, added as a *pre-open* hook so the Linux
  `FICLONE` path stays exactly where it is (S1.5).
- **Trash through `NSFileManager`, restore through our own journal.** macOS has no
  `.trashinfo`; `trashItemAtURL:resultingItemURL:` tells us where the item went, and
  a journal file in the state directory maps that back to the original path. Items
  Finder trashed are not listed; "Empty trash" empties only what the journal knows
  and says so.
- **Clipboard is the synchronous fallback API** (S1.23) with `NSPasteboard` bodies.
  No event-driven ownership: AppKit copies the data on `setData`, so there is no
  server process to keep alive and `copy()` returns `Ok(None)`.
- **Drag-out via `NSDraggingSession`**, started from the `CursorMoved` arm rather
  than from the frame tick, because AppKit wants the drag begun inside the mouse
  event that is dragging. **Drop-in via winit** (S1.32) plus a pointer-position hook.
- **Volumes via `NSFileManager.mountedVolumeURLs…` and `NSWorkspace` unmount/eject.**
  No mounting of unmounted disks (macOS automounts; the row simply is not there).
  Network shares are the non-local volumes; "Connect to" hands the URL to
  `NSWorkspace.openURL`, which is Finder's own connect flow.
- **Cmd is Ctrl for chord matching.** Keymap files stay portable, labels render `⌘`.
  A short macOS override table in `05-defaults-and-config.md` §3 gives Cmd+C/V/W
  their expected meanings. Option+letter chords match because `keys.rs` uses
  `key_without_modifiers`; the composed character (`π` for Option+p) is only
  suppressed when the window is created with `OptionAsAlt::Both` (S1.27 does this),
  at the cost of dead-key composition in prompts — logged below.
- **Hidden files: dot-prefix only** (`~/Library` is shown; `UF_HIDDEN` is ignored).
- **Software video decode in this phase.** dv-media is vendored unchanged; a
  VideoToolbox row is an optional task at the end.
- **objc2 crates only**, pinned to winit's versions (`00-ground-rules.md` §3).
  Everything else is `libc`.

## 1. Filesystem (df-core `platform/macos/`)

- [ ] **M2.1** `platform::watch` kqueue backend in `platform/macos/watch.rs`:
      `kqueue()`; per watched directory `open(dir, O_EVTONLY)` and
      `EV_SET(fd, EVFILT_VNODE, EV_ADD|EV_CLEAR, NOTE_WRITE|NOTE_DELETE|NOTE_RENAME|
      NOTE_REVOKE|NOTE_ATTRIB|NOTE_EXTEND)`; a wake pipe like the Linux design's,
      but made with `pipe()` + `fcntl(F_SETFD, FD_CLOEXEC)` + `fcntl(F_SETFL,
      O_NONBLOCK)` because apple libc has no `pipe2` (the Linux `Pipe` type does not
      compile there), registered as `EVFILT_READ`. `NOTE_WRITE|NOTE_EXTEND|NOTE_ATTRIB` on a
      directory → `WatchEvent::Changed(dir)`; `NOTE_DELETE|NOTE_RENAME|NOTE_REVOKE`
      → `Gone(dir)` and the fd is closed; a `kevent` return of `-1` with `EINTR` is
      retried. The `DEBOUNCE` (`watch.rs:52`) applies unchanged. Same `Backend`
      surface as S1.4; `#![allow(unsafe_code)]` with the three ownership rules
      restated. Done when: the three `Watcher` tests from `fs/tests.rs` (S1.4 gated
      them to Linux) are re-enabled under `#[cfg(any(target_os = "linux",
      target_os = "macos"))]` and pass on the macOS runner.
- [ ] **M2.2** `platform::fs::clone_before_open(src, dst) -> io::Result<bool>`:
      macOS body `libc::fclonefileat(src_fd, AT_FDCWD, dst_cstr, CLONE_NOFOLLOW)`
      after `open(src, O_RDONLY)`, with `const CLONE_NOFOLLOW: u32 = 0x0001` declared
      locally (from `<sys/clonefile.h>`; libc 0.2.189 has `fclonefileat` but not the
      flag); `Ok(true)` on success, `Ok(false)` on
      `ENOTSUP`/`EXDEV`/`EINVAL` (cross-volume, non-APFS, or a destination that
      exists — the caller has already resolved conflicts so `EEXIST` is a real
      error). Linux and Windows bodies return `Ok(false)`. `ops/copy.rs::copy_file`
      calls it before creating the writer and skips the chunked copy on `true`;
      `apply_mode`/`set_times` still run (a clone carries them, so they are
      idempotent). Done when: a test on the macOS runner copies a 100 MiB file and
      asserts the copy took under 50 ms; skipped on other targets.
- [ ] **M2.3** `platform::fs::is_remote`: `statfs` and `f_fstypename` in
      `{"nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp"}` or starting with
      `"fuse"`/`"macfuse"`/`"osxfuse"`. `magic_of` returns `f_type` for parity.
      Done when: a unit test with a fake `statfs` result table passes.
- [ ] **M2.4** `platform::user::owner_names`: macOS body uses `getpwuid_r`/`getgrgid_r`
      (Open Directory serves them; `/etc/passwd` lists only system accounts). The
      Linux body keeps its `/etc/passwd` parser. Done when: `fs::owner` tests pass on
      both.
- [ ] **M2.5** `platform::thread::lower_priority`: macOS body
      `pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0)` for `nice > 0`, leaving
      the calling thread's class at UTILITY. Neither the function nor
      `QOS_CLASS_UTILITY` (`0x11`) is in libc 0.2.189: declare
      `extern "C" { fn pthread_set_qos_class_self_np(cls: u32, rel: i32) -> i32; }`
      and the constant locally, in the platform file. Done when: compiles and a
      test asserts the call returns 0.
- [ ] **M2.6** `platform::process::rsync_available()` gates on version: parse
      `rsync --version`'s first line and require ≥ 3.1.0 (`--info=progress2`);
      `platform::process::RSYNC_HINT` = "needs rsync 3.1 or newer — `brew install
      rsync`" and `app/syncing.rs:123` appends it to its error. Done when: the
      pure parser test covers `rsync  version 2.6.9`, `3.2.7` and openrsync's
      banner (returns false).
- [ ] **M2.7** `platform::fs::forget_cached`: `fcntl(fd, F_NOCACHE, 1)` is *not*
      used (it changes the file's caching mode, not a hint); the macOS body stays a
      no-op as in S1.5. Done when: recorded here; nothing to do.

## 2. Trash (df-core `platform/macos/trash.rs`)

- [ ] **M2.8** `Trash::home()` returns a `Trash` whose root is the journal file
      `platform::dirs::state_dir()/trash-journal`; `Trash::trash(path, ctx)` calls
      `NSFileManager.defaultManager.trashItemAtURL:resultingItemURL:error:`
      (objc2-foundation), then appends a journal line
      `<deleted_at>\t<original>\t<location>` (UTF-8, escaped like the state file)
      and returns `TrashedItem { name: location.file_name(), original, deleted_at,
      location }`. `list()` reads the journal and keeps lines whose `location` still
      exists (dropping stale ones on the next write). `restore` renames `location`
      back to `original` (or `move_cross_device`) and removes the line; `purge`
      removes `location` and the line. `for_path` returns `home()` for every local
      path (NSFileManager picks the volume's `.Trashes` itself). `available_for` is
      true for local, non-remote paths. Done when: a test on the macOS runner
      trashes a temp file, sees it under `~/.Trash`, restores it, and the journal
      is empty again.
- [ ] **M2.9** Trash view and "Empty trash" on macOS: `App::show_trash`
      (`app.rs:4055–4078`) lists the journal; the empty-state text says "Only
      files trashed from delightfile are listed — Finder's Trash may hold more";
      "Empty trash" purges journal items only and the toast says so. Done when: a
      `App::for_test` test on macOS shows the text.

## 3. Clipboard, drag and drop (df-app `platform/macos/`)

- [ ] **M2.10** `platform::clipboard` bodies (S1.23 surface) with
      `NSPasteboard.generalPasteboard` (objc2-app-kit):
      - `copy(None, bytes)` → `clearContents`, `setString:forType:NSPasteboardTypeString`.
      - `copy(Some("text/uri-list"), bytes)` → parse the list with the existing
        `parse_uri_list`, `writeObjects:` an `NSArray<NSURL>` (file URLs).
      - `copy(Some(image/*), bytes)` → `setData:forType:` with the UTI for the mime
        (`public.png`, `public.jpeg`, `public.tiff`, `com.compuserve.gif`,
        `org.webmproject.webp`); unknown image mimes go as `public.png` only if the
        bytes are PNG, else `Err(ClipError::Failed)`.
      - Returns `Ok(None)` (no child).
      - `offered_types()` → the pasteboard's `types`, mapped: `public.file-url` →
        `text/uri-list`, `public.utf8-plain-text` → `text/plain;charset=utf-8`,
        `public.png` → `image/png`, `public.tiff` → `image/tiff`, `public.jpeg` →
        `image/jpeg`; order preserved.
      - `paste("text/uri-list")` → `readObjectsForClasses:[NSURL] options:{FileURLsOnly}`
        rendered as a CRLF `file://` list through `clipboard::uri_list`, so the
        existing parser is the only parser. `paste(text/*)` → `stringForType:`.
        `paste(image/*)` → `dataForType:` of the matching UTI.
      Done when: unit tests for the mime↔UTI tables pass everywhere; a macOS-runner
      test round-trips a string and a file URL through the real pasteboard.
- [ ] **M2.11** `platform::desktop::pointer_position(&Window) -> Option<(f32, f32)>`:
      macOS body `NSWindow.mouseLocationOutsideOfEventStream` converted to the
      view's top-left logical coordinates (flip y by the content view's height).
      Used by S1.32's drop-in arms. Done when: a drop from Finder highlights the
      row under the pointer (live check V7 §4.5).
- [ ] **M2.12** Drag-out: `platform::desktop::Desktop::drag(offers, count, card,
      ink, scale)` on macOS builds `NSPasteboardItem`s — one per path with
      `public.file-url` (the URL string) plus one item carrying
      `public.utf8-plain-text` (paths joined by `\n`, as `dnd::offer` already
      provides) — wraps each in an `NSDraggingItem` whose image is an `NSImage`
      made from `platform::icon::draw(count, card, ink)` (the icon is premultiplied
      BGRA; swizzle to RGBA and hand it to `NSBitmapImageRep` with `bitmapFormat: 0`,
      whose default *is* premultiplied — do not un-premultiply; `HOTSPOT` sets the
      frame origin). Also offer `dnd::SELF_MIME` as a custom pasteboard type so a
      future NSView drop override could recognise our own drags; S1.32's winit path
      cannot see it and treats every drop as external. Then call
      `NSView.beginDraggingSessionWithItems:event:source:` on the winit `NSView`
      (`RawWindowHandle::AppKit.ns_view`) with `NSApp.currentEvent`. The `source`
      is an objc2 0.5 `declare_class!` `DfDragSource: NSObject <NSDraggingSource>` with
      `draggingSession:sourceOperationMaskForDraggingContext:` → `Copy` and
      `draggingSession:endedAtPoint:operation:` → pushes `Event::DragEnded`. Because
      the session must start inside the mouse event, `App::tick_drag`'s hand-off
      check (`app.rs:12458–12461`) is mirrored in the `CursorMoved` arm on macOS:
      `platform::desktop::HANDS_OFF_ON_CURSOR_MOVED` (const, macOS `true`) makes
      `window_event` call `hand_off_drag` there and `tick_drag` skip it. Linux
      keeps the tick path. `Desktop::start` on macOS returns `Some` (it holds the
      `ns_view` and the event queue). Done when: a drag out of the list onto the
      Desktop makes a copy (live check); unit test that `offer` → pasteboard items
      count matches.
- [ ] **M2.13** `Desktop::set_selection`/`receive` on macOS delegate to M2.10's
      synchronous functions and answer `Event::Copied { ok: true }` /
      `Event::Pasted` immediately on the next `poll`, so `app.rs`'s state machine
      sees the same events as on Linux. `Event::Selection` is emitted on `poll`
      when `NSPasteboard.changeCount` moved (mirrors the clipboard types into
      `App.clipboard_types` for the paste chord's readiness). Done when: `y` then
      Cmd+V in Finder and Cmd+C in Finder then `p` both work (live check).

## 4. Volumes and connect (df-app `platform/macos/mounts.rs`)

- [ ] **M2.14** `Mounts::start` worker: `Request::List` → `NSFileManager
      .mountedVolumeURLsIncludingResourceValuesForKeys:options:` with keys
      `NSURLVolumeNameKey`, `NSURLVolumeTotalCapacityKey`,
      `NSURLVolumeIsRemovableKey`, `NSURLVolumeIsEjectableKey`,
      `NSURLVolumeIsInternalKey`, `NSURLVolumeIsLocalKey`,
      `NSURLVolumeLocalizedFormatDescriptionKey`, options
      `NSVolumeEnumerationSkipHiddenVolumes`. Local volumes → `Device { object:
      mount path, drive: None, node: statfs f_mntfromname, label, fs, size,
      mount: Some(path), removable, ejectable, hardware: "" }`; non-local → `Share
      { url: statfs f_mntfromname, label, scheme: from the URL, path }`.
      `Unmount`/`Eject`/`UnmountShare` → `NSWorkspace.sharedWorkspace
      .unmountAndEjectDeviceAtURL:error:`; `Mount` → `Reply::Failed("macOS mounts
      disks itself")`. Done when: unit tests for the row mapping pass with a fake
      table; live check V7 §4.7.
- [ ] **M2.15** `connect(url)`: `NSWorkspace.openURL` on the `smb://`,
      `sftp://` (Finder does not mount sftp; for `sftp` return
      `Connected::Failed("use the sftp: bookmark instead")`), `nfs://`, `ftp://`
      URL (`afp` is not offered: `mounts::SCHEMES` at `mounts.rs:273` decides what
      the prompt accepts and stays as it is); then poll `/Volumes` for a new entry for up to 10 s and return
      `Connected::Mounted(Some(path))` or `Mounted(None)`. `TERMINAL_MOUNT` is
      `None`. Done when: live check.

## 5. Openers, fonts, dirs (see `05-defaults-and-config.md` for the tables)

- [ ] **M2.16** `platform::open::detached_argv` on macOS: no `setsid`; instead
      `spawn_detached` uses `CommandExt::process_group(0)` and null stdio so the
      child outlives the window. Done when: a test spawns `sleep 1` and the app's
      exit does not kill it (macOS runner).
- [ ] **M2.17** The macOS default opener and rule tables from `05-defaults-and-config.md`
      §2 are wired into `platform::defaults::{OPENERS, RULES, BOOKMARKS}` and
      `df_core::config::Config::default` reads them. Done when: `open.rs`'s
      `opener_rules_pick_by_glob_then_mime` has a macOS twin asserting `open "$1"`.
- [ ] **M2.18** `platform::dirs` macOS values per `05-defaults-and-config.md` §1
      (D5.1 lands them; this task is the cross-reference). Done when: D5.1 done.
- [ ] **M2.19** `platform::fonts::dirs()` macOS list (D5.6) and the README line
      telling people `brew install --cask font-symbols-only-nerd-font`. Done when:
      the icon font is found on a machine with that cask (live check).

## 6. Keyboard and window

- [ ] **M2.20** `platform::keys::mods(state: ModifiersState) -> Mods`: macOS body
      sets `ctrl = control_key() || super_key()` and `super_key = false`; other
      targets keep `keys.rs:52–66` as today. `keys::chord_from` calls it. Done when:
      a `keys.rs` test with `SUPER` held maps to `ctrl` under `cfg!(target_os =
      "macos")` and to `super_key` elsewhere.
- [ ] **M2.21** Labels: `df_core::keymap::Chord::label` (`keymap/key.rs:323–345`)
      takes a `df_core::platform::keys::LABELS` table (pure strings, in df-core —
      df-core has no winit, so this is a separate module from df-app's
      `platform::keys::mods`): macOS `⌃`/`⌥`/`⇧`/`⌘` with `Ctrl+`
      rendered as `⌘` (since Cmd is the primary), Linux/Windows unchanged. The six
      hard-coded UI strings (appendix B §3: `cli.rs:170`, `overlay.rs:549`,
      `app.rs:12393, 12845, 16480, 16781`) go through `df_core::platform::keys::ctrl_name()`
      ("Ctrl" / "⌘"). Done when: `keys.rs:164`'s `"Alt+←"` assertion has a macOS
      twin `"⌥←"`.
- [ ] **M2.22** Pointer modifiers: `app.rs:13785–13787` already treats egui's
      `command` (Cmd) as `toggle`; confirm `dnd::verb_for` gets Cmd → Copy and
      Option → Link on macOS and add the mapping to the help sheet's mouse section.
      Done when: reviewed; live check.
- [ ] **M2.23** Occlusion path (`app.rs:158–167, 16217–16236`, `graphics.rs:38–67`):
      the comments say the code is right and dead on Wayland. On macOS it is live.
      No code change; add a `log::debug!` when `Presented::Occluded` fires so the
      live check can confirm it. Done when: the log line exists; V7 §4.1 minimize
      check.
- [ ] **M2.24** Present mode: Metal has no `Mailbox`; `graphics.rs:148–152` takes
      `Fifo`. The animation pacing assumption (frames paced to refresh under
      Mailbox) needs a live check for stutter; no code change unless the check
      fails. Done when: recorded; V7 §4.1.
- [ ] **M2.25** `Windows::open` (`window.rs:122–141`) on macOS spawns
      `current_exe()` directly, which works from inside the bundle; each window is
      its own Dock tile (accepted decision). Add `-n`-style behaviour nowhere. Done
      when: live check of Ctrl/Cmd+N.
- [ ] **M2.26** Cmd+Q: winit 0.30.13's default menu binds Quit to `terminate:`
      (`menu.rs:71`); its app delegate implements only `applicationWillTerminate:`,
      which calls `exiting()` on the handler — **not** `CloseRequested`, which only
      `windowShouldClose:` sends. So Cmd+Q runs `App::exiting` (`app.rs:17209–17262`,
      which flushes state) but not `App::finish` (`app.rs:16345–16367`: chooser/cwd
      files, `data_device = None`). Task: make `exiting` perform everything `finish`
      does when `finish` has not run (a `finished: bool`), so the Cmd+Q path loses
      nothing; the alternative — a custom delegate implementing
      `applicationShouldTerminate:` to route through `CloseRequested` — is the
      fallback if the first proves insufficient. Done when: a unit test drives
      `exiting` without `finish` and sees the cwd file written; live check V7 §4.1.

## 7. Optional

- [~] **M2.27** VideoToolbox hardware decode: add a
      `(AV_HWDEVICE_TYPE_VIDEOTOOLBOX, DecodePath::VideoToolbox)` row to
      `dv-media/src/decode.rs:601–610`'s `PROBE_ORDER` under `cfg(target_os =
      "macos")`. This diverges the vendored crate from delightviewer; do it only if
      the live check finds 4K playback unusable, and note the divergence in
      `crates/dv-media/Cargo.toml`'s header comment.

## Decisions log

- 2026-09-25 — kqueue over FSEvents (coarse event model, no new FFI).
- 2026-09-25 — Trash via NSFileManager with a private restore journal; Finder's own
  items are not listed.
- 2026-09-25 — Drag-out started from `CursorMoved` on macOS; Linux keeps the tick.
- 2026-09-25 — Cmd is Ctrl for matching; `⌘` labels; small override table in Phase 5.
- 2026-09-25 — No disk mounting on macOS; connect goes through `NSWorkspace.openURL`.
- 2026-09-25 — `OptionAsAlt::Both` on the window: Option is a modifier, not a
  composition key. Dead-key composition (Option+e, e → é) is lost in prompts; typing
  accented text there needs the character viewer or a paste. Accepted for a
  keyboard-driven file manager whose prompts are mostly file names.

## Open questions

- Whether `NSApp.currentEvent` inside winit's `CursorMoved` dispatch is the
  `mouseDragged` event AppKit requires for `beginDraggingSession` (M2.12). If not,
  the fallback is an `NSView` subclass override of `mouseDragged:` — record which.
- Ghostty on macOS: does `ghostty -e` work from `open -a`? Affects the `edit` opener
  default in `05-defaults-and-config.md`.
