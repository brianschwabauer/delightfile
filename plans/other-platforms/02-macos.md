# 02 — macOS

Status: **in progress**

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

- [x] **M2.1** `platform::watch` kqueue backend in `platform/macos/watch.rs`:
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
      target_os = "macos"))]` and pass on the macOS runner. — done e50b384, green on
      the macOS runner (run 36640376030)
- [x] **M2.2** `platform::fs::clone_before_open(reader, dst) -> io::Result<bool>`:
      macOS body `libc::fclonefileat(reader_fd, AT_FDCWD, dst_cstr,
      CLONE_NOFOLLOW)` on the source the copy has open, with
      `const CLONE_NOFOLLOW: u32 = 0x0001` declared
      locally (from `<sys/clonefile.h>`; libc 0.2.189 has `fclonefileat` but not the
      flag); `Ok(true)` on success, `Ok(false)` on
      `ENOTSUP`/`EXDEV`/`EINVAL` (cross-volume, non-APFS, or a destination that
      exists — the caller has already resolved conflicts so `EEXIST` is a real
      error). Linux and Windows bodies return `Ok(false)`. `ops/copy.rs::copy_file`
      calls it before creating the writer and skips the chunked copy on `true`;
      `apply_mode`/`set_times` still run (a clone carries them, so they are
      idempotent). Done when: a test on the macOS runner copies a 100 MiB file and
      asserts the copy took under 50 ms; skipped on other targets. — done 13ed23f,
      green on the macOS runner (run 36640376030)
- [x] **M2.3** `platform::fs::is_remote`: `statfs` and `f_fstypename` in
      `{"nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp"}` or starting with
      `"fuse"`/`"macfuse"`/`"osxfuse"`. `magic_of` returns `f_type` for parity.
      Done when: a unit test with a fake `statfs` result table passes. — done
      bd40078, green on the macOS runner (run 36640376030)
- [x] **M2.4** `platform::user::owner_names`: macOS body uses `getpwuid_r`/`getgrgid_r`
      (Open Directory serves them; `/etc/passwd` lists only system accounts). The
      Linux body keeps its `/etc/passwd` parser. Done when: `fs::owner` tests pass on
      both. — done fb4b9d8, green on the macOS runner (run 36640376030)
- [x] **M2.5** `platform::thread::lower_priority`: macOS body
      `pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0)` for `nice > 0`, leaving
      the calling thread's class at UTILITY. Both are in libc 0.2.189
      (`libc::pthread_set_qos_class_self_np`, `libc::qos_class_t::QOS_CLASS_UTILITY`,
      re-exported from its `new/apple/libpthread`), so nothing is declared
      locally. Done when: compiles and a test asserts the call returns 0. — done
      74850e7, green on the macOS runner (run 36640376030)
- [x] **M2.6** `platform::process::rsync_available()` gates on version: parse
      `rsync --version`'s first line and require ≥ 3.1.0 (`--info=progress2`);
      `platform::process::RSYNC_HINT` = "needs rsync 3.1 or newer — `brew install
      rsync`" and `app/syncing.rs:123` appends it to its error. Done when: the
      pure parser test covers `rsync  version 2.6.9`, `3.2.7` and openrsync's
      banner (returns false). — done 5521a89, green on the macOS runner (run
      36640376030); the df-app half, appending `RSYNC_HINT` to the refusal in
      `app/syncing.rs`, is the df-app branch's
- [x] **M2.7** `platform::fs::forget_cached`: `fcntl(fd, F_NOCACHE, 1)` is *not*
      used (it changes the file's caching mode, not a hint); the macOS body stays a
      no-op as in S1.5. Done when: recorded here; nothing to do. — done bd40078,
      green on the macOS runner (run 36640376030)

## 2. Trash (df-core `platform/macos/trash.rs`)

- [x] **M2.8** `Trash::home()` returns a `Trash` whose root is the journal file
      `platform::dirs::state_dir()/delightfile/trash-journal`;
      `Trash::trash(path, ctx)` calls
      `NSFileManager.defaultManager.trashItemAtURL:resultingItemURL:error:`
      (objc2-foundation), then appends a journal line
      `<deleted_at>\t<original>\t<location>` (UTF-8, escaped like the state file)
      and returns `TrashedItem { trash_root: the journal, name:
      location.file_name(), original, deleted_at }`, whose `location()` reads its
      line back from the journal. `list()` reads the journal and keeps lines
      whose `location` still
      exists (dropping stale ones on the next write). `restore` renames `location`
      back to `original` (or `move_cross_device`) and removes the line; `purge`
      removes `location` and the line. `for_path` returns `home()` for every local
      path (NSFileManager picks the volume's `.Trashes` itself). `available_for` is
      true for local, non-remote paths. Done when: a test on the macOS runner
      trashes a temp file, sees it under `~/.Trash`, restores it, and the journal
      is empty again. — done df45c91, green on the macOS runner (run 36640376030)
- [x] **M2.9** Trash view and "Empty trash" on macOS: `App::show_trash`
      (`app.rs:4055–4078`) lists the journal; the empty-state text says "Only
      files trashed from delightfile are listed — Finder's Trash may hold more";
      "Empty trash" purges journal items only and the toast says so. Done when: a
      `App::for_test` test on macOS shows the text.
      — done e4d16f4, compiles and unit-tested on the macOS runner, not seen on
      screen. The words are df-app's `platform::trash::{LISTED_NOTE, EMPTIED_NOTE}`
      (`None` on Linux and Windows), read by `trashview::empty_note` under an empty
      view and by `trashview::purged_whole` for "Empty trash"'s toast;
      `app::tests::an_empty_trash_says_what_it_lists` opens the view on a sandbox
      trash and reads the macOS line on the runner. df-core's M2.8 is in since the
      two branches were integrated, so `Trash::home()` is the journal and `d`,
      `g t` and "Empty trash" are live on macOS, with these words.

## 3. Clipboard, drag and drop (df-app `platform/macos/`)

- [x] **M2.10** `platform::clipboard` bodies (S1.23 surface) with
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
      — done e24e58a (its image test corrected in d2a7c93), compiles and unit-tested
      on the macOS runner, not seen on screen. Text, a list of two files and a PNG
      round-trip through a pasteboard of the test's own (`pasteboardWithUniqueName`,
      served by the same pasteboard server as the general one, so a developer's
      clipboard is left alone); the mime↔UTI table is `platform::pasteboard`, tested
      on every target. Read back item by item rather than with
      `readObjectsForClasses:` (Decisions log).
- [x] **M2.11** `platform::desktop::pointer_position(&Window) -> Option<(f32, f32)>`:
      macOS body `NSWindow.mouseLocationOutsideOfEventStream` converted to the
      view's top-left logical coordinates (flip y by the content view's height).
      Used by S1.32's drop-in arms. Done when: a drop from Finder highlights the
      row under the pointer (live check V7 §4.5).
      — done db263f0, compiles and unit-tested on the macOS runner, not seen on
      screen: winit's view is flipped, so `convertPoint:fromView:` of the window's
      point is the flip. Live check 07-verification.md §4.5.
- [x] **M2.12** Drag-out: `platform::desktop::Desktop::drag(offers, count, card,
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
      — done f528bec, compiles and unit-tested on the macOS runner, not seen on
      screen. `dragout::tests::every_file_is_one_item` checks the item count against
      `dnd::offer` on every target. Each file item carries its path as text rather
      than one extra text item, and the picture is drawn at scale 1 (Decisions log).
      Whether `currentEvent` inside winit's `CursorMoved` is the `mouseDragged`
      AppKit wants stays an Open question for the live check (§4.5).
- [x] **M2.13** `Desktop::set_selection`/`receive` on macOS delegate to M2.10's
      synchronous functions and answer `Event::Copied { ok: true }` /
      `Event::Pasted` immediately on the next `poll`, so `app.rs`'s state machine
      sees the same events as on Linux. `Event::Selection` is emitted on `poll`
      when `NSPasteboard.changeCount` moved (mirrors the clipboard types into
      `App.clipboard_types` for the paste chord's readiness). Done when: `y` then
      Cmd+V in Finder and Cmd+C in Finder then `p` both work (live check).
      — done e806cf9, compiles and unit-tested on the macOS runner, not seen on
      screen. The mirror reads `changeCount` at most every 200 ms (Decisions log).
      Live check §4.5.

## 4. Volumes and connect (df-app `platform/macos/mounts.rs`)

- [x] **M2.14** `Mounts::start` worker: `Request::List` → `NSFileManager
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
      — done 0ef1310, compiles and unit-tested on the macOS runner, not seen on
      screen: `platform::volumes::rows` maps a fake table on every target, and
      `the_boot_volume_is_listed_as_a_disk` finds `/` on the runner. An ejectable
      disk's `drive` is its mount point, and a share's URL is
      `NSURLVolumeURLForRemountingKey` where the system has one (Decisions log).
      Live check §4.7.
- [x] **M2.15** `connect(url)`: `NSWorkspace.openURL` on the `smb://`,
      `sftp://` (Finder does not mount sftp; for `sftp` return
      `Connected::Failed("use the sftp: bookmark instead")`), `nfs://`, `ftp://`
      URL (`afp` is not offered: `mounts::SCHEMES` at `mounts.rs:273` decides what
      the prompt accepts and stays as it is); then poll `/Volumes` for a new entry for up to 10 s and return
      `Connected::Mounted(Some(path))` or `Mounted(None)`. `TERMINAL_MOUNT` is
      `None`. Done when: live check.
      — done d29643e, compiles and unit-tested on the macOS runner, not seen on
      screen for what can run there (the `sftp://` refusal, the routing table);
      nothing connects on a runner. `dav://` and `davs://` are refused too
      (Decisions log); what `Mounted(None)` says is an Open question. Live check
      §4.7.

## 5. Openers, fonts, dirs (see `05-defaults-and-config.md` for the tables)

- [x] **M2.16** `platform::open::detached_argv` on macOS: no `setsid`; instead
      `spawn_detached` uses `CommandExt::process_group(0)` and null stdio so the
      child outlives the window. Done when: a test spawns `sleep 1` and the app's
      exit does not kill it (macOS runner).
      — done e89693f, compiles and unit-tested on the macOS runner, not seen on
      screen: `a_launched_program_leads_its_own_group_and_is_collected` sees the
      child lead a process group of its own, outlive the call, and leave no zombie.
      The shared Unix spawn asks `platform::open::{detach, release}`; on Linux both
      do nothing (Decisions log).
- [~] **M2.17** The macOS default opener and rule tables from `05-defaults-and-config.md`
      §2 are wired into `platform::defaults::{OPENERS, RULES, BOOKMARKS}` and
      `df_core::config::Config::default` reads them. Done when: `open.rs`'s
      `opener_rules_pick_by_glob_then_mime` has a macOS twin asserting `open "$1"`.
      — blocked: every half of it is df-core's (`Config::default` in `config.rs`,
      `platform::defaults` beside it), and this branch was told to keep out of
      df-core while two other branches change it. Options: (a) the df-core branch,
      or a pass after the three merge, adds `df_core::platform::defaults::{OPENERS,
      RULES, BOOKMARKS}` per target from 05 §2.1/§6 and `Config::default` reads
      them, and df-app then gains the `open.rs` twin asserting `open "$1"`; (b)
      Brian lets this branch make that df-core change itself, at the cost of a
      likely conflict in `config.rs` with `port/paths`.
- [x] **M2.18** `platform::dirs` macOS values per `05-defaults-and-config.md` §1
      (D5.1 lands them; this task is the cross-reference). Done when: D5.1 done. —
      done 9041f14, green on the macOS runner (run 36640376030); the macOS column of
      D5.1's table for `platform::dirs` (zoxide's row is not `platform::dirs` and
      stays with D5.1)
- [x] **M2.19** `platform::fonts::dirs()` macOS list (D5.6) and the README line
      telling people `brew install --cask font-symbols-only-nerd-font`. Done when:
      the icon font is found on a machine with that cask (live check).
      — done 14120fe (with D5.6), compiles and unit-tested on the macOS runner, not
      seen on screen; the README says how to get the cask. Live check that the icon
      font is found (§4.1).

## 6. Keyboard and window

- [x] **M2.20** `platform::keys::mods(state: ModifiersState) -> Mods`: macOS body
      sets `ctrl = control_key() || super_key()` and `super_key = false`; other
      targets keep `keys.rs:52–66` as today. `keys::chord_from` calls it. Done when:
      a `keys.rs` test with `SUPER` held maps to `ctrl` under `cfg!(target_os =
      "macos")` and to `super_key` elsewhere.
      — done 925ed12, compiles and unit-tested on the macOS runner, not seen on
      screen: `keys::tests::super_is_ctrl_on_macos_and_itself_elsewhere`.
- [~] **M2.21** Labels: `df_core::keymap::Chord::label` (`keymap/key.rs:323–345`)
      takes a `df_core::platform::keys::LABELS` table (pure strings, in df-core —
      df-core has no winit, so this is a separate module from df-app's
      `platform::keys::mods`): macOS `⌃`/`⌥`/`⇧`/`⌘` with `Ctrl+`
      rendered as `⌘` (since Cmd is the primary), Linux/Windows unchanged. The six
      hard-coded UI strings (appendix B §3: `cli.rs:170`, `overlay.rs:549`,
      `app.rs:12393, 12845, 16480, 16781`) go through `df_core::platform::keys::ctrl_name()`
      ("Ctrl" / "⌘"). Done when: `keys.rs:164`'s `"Alt+←"` assertion has a macOS
      twin `"⌥←"`.
      — blocked: `Chord::label` and the `LABELS` table are df-core's (see M2.17 for
      why this branch did not touch df-core). The df-app half is the strings that
      name Ctrl, today overlay.rs:612 ("Ctrl+s to stop"), overlay.rs:1194 (`Ctrl+{n}`),
      app.rs:14814 ("Ctrl+N opens another window"), app.rs:15309 ("press Ctrl+v
      again"), app.rs:19733 (the `Ctrl+s` hint) and app.rs:20139 ("Ctrl-click
      selects"), plus cli.rs:205's `Ctrl+Enter`. Options: (a) df-core gains
      `platform::keys::{LABELS, ctrl_name}` and `Chord::label` reads them, and then
      the df-app strings go through `ctrl_name()`; (b) df-app renders those strings
      through a `platform::keys` name of its own now, with chord labels still saying
      `Ctrl+` on macOS until (a), which would show `⌘` and `Ctrl` side by side
      meanwhile.
- [x] **M2.22** Pointer modifiers: `app.rs:13785–13787` already treats egui's
      `command` (Cmd) as `toggle`; confirm `dnd::verb_for` gets Cmd → Copy and
      Option → Link on macOS and add the mapping to the help sheet's mouse section.
      Done when: reviewed; live check.
      — done, nothing to commit (reviewed 2026-09-29). `pointer.toggle` is egui's
      `command || ctrl`, and egui-winit sets `command` from Cmd on macOS, so
      Cmd-click toggles and Cmd-drag is `Verb::Copy`; Option is egui's `alt`, so
      Option-drag is `Verb::Link`. The help sheet has no mouse section to add the
      mapping to; its "⌘-click toggles, ⌥-drag links" line is D5.5's and blocked
      with it. Live check §4.5.
- [x] **M2.23** Occlusion path (`app.rs:158–167, 16217–16236`, `graphics.rs:38–67`):
      the comments say the code is right and dead on Wayland. On macOS it is live.
      No code change; add a `log::debug!` when `Presented::Occluded` fires so the
      live check can confirm it. Done when: the log line exists; V7 §4.1 minimize
      check.
      — done, nothing to commit: `Gfx::present` already logs "surface occluded;
      frames paused until the window is shown" and "surface visible again" at debug,
      once per change. Live check §4.1 with `RUST_LOG=debug`.
- [x] **M2.24** Present mode: Metal has no `Mailbox`; `graphics.rs:148–152` takes
      `Fifo`. The animation pacing assumption (frames paced to refresh under
      Mailbox) needs a live check for stutter; no code change unless the check
      fails. Done when: recorded; V7 §4.1.
      — recorded, nothing to commit: `Gfx::new` takes `Mailbox` only when the
      surface offers it, so Metal gets `Fifo`. Live check §4.1.
- [x] **M2.25** `Windows::open` (`window.rs:122–141`) on macOS spawns
      `current_exe()` directly, which works from inside the bundle; each window is
      its own Dock tile (accepted decision). Add `-n`-style behaviour nowhere. Done
      when: live check of Ctrl/Cmd+N.
      — recorded, nothing to commit: `Windows::open` spawns `current_exe()`, which
      inside the bundle is `Contents/MacOS/delightfile`. Live check §4.1.
- [x] **M2.26** Cmd+Q: winit 0.30.13's default menu binds Quit to `terminate:`
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
      — done 6fe6213, compiles and unit-tested on the macOS runner, not seen on
      screen: `app::tests::an_exit_without_a_close_still_writes_the_cwd_file`. The
      first route was enough; no application delegate (Decisions log). Live check
      §4.1.
- [x] **M2.30** `platform::appearance` macOS body (S1.35's surface: `Desktop`,
      `Connect`, `session`): the system's light or dark for `[flavor] mode =
      "auto"`, in place of the stub that answers nothing and says `Link::Gone`.
      Two routes, to be chosen here and logged: winit 0.30.13 already reports the
      system theme on macOS with no new FFI (`Window::theme()`, and
      `WindowEvent::ThemeChanged`, which winit documents as unsupported only on
      iOS/Android/X11/Wayland/Orbital), but the seam asks before the window
      exists (`App::new`) and `App::window_theme` sets the window's own theme,
      after which `ThemeChanged` no longer reports; or
      `NSApp.effectiveAppearance` through objc2-app-kit 0.2 with a key-value
      observer on the app, which needs no window. Done when: a unit test of the
      appearance-name → `Scheme` mapping passes on the macOS runner; live check
      that switching System Settings → Appearance turns a window in `auto`
      (V7 §4.1).
      — done fb1923c, compiles and unit-tested on the macOS runner, not seen on
      screen: the `effectiveAppearance` route, with key-value observing, shipped,
      not the fallback (Decisions log). Live check §4.1.

## 7. Optional

- [~] **M2.27** VideoToolbox hardware decode: add a
      `(AV_HWDEVICE_TYPE_VIDEOTOOLBOX, DecodePath::VideoToolbox)` row to
      `dv-media/src/decode.rs:601–610`'s `PROBE_ORDER` under `cfg(target_os =
      "macos")`. This diverges the vendored crate from delightviewer; do it only if
      the live check finds 4K playback unusable, and note the divergence in
      `crates/dv-media/Cargo.toml`'s header comment.

## 8. Found in Phase 1

- [x] **M2.28** `platform::nofollow` macOS body (S1.19), in place of the stub
      that refuses every permissions change: the same anchored walk without
      `/proc` — `open(anchor, O_RDONLY | O_DIRECTORY)`, then each component with
      `openat(dirfd, name, O_RDONLY | O_NOFOLLOW | O_DIRECTORY)` (macOS has no
      `O_PATH`, so a folder must be readable to be walked; one shut to its owner
      is the "Apply again" case `ops::mode` already reports),
      `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` for what the last component is,
      and `fchmodat(dirfd, name, mode, AT_SYMLINK_NOFOLLOW)` to set it, which
      never follows a link (on one swapped in after the lookup it would set the
      link's own mode). `read_dir_in` lists through the descriptor (`fdopendir` of
      a `dup`, rewound). Done when: `ops/mode.rs`'s
      `on_disk` tests are `#[cfg(unix)]` and pass on the macOS runner. — done
      1bf689e, green on the macOS runner (run 36640376030)
- [x] **M2.29** `platform::process::tie_to_this_thread` macOS body (S1.50), in
      place of the no-op under which an rclone daemon outlives a delightfile that
      crashes. macOS has no `PR_SET_PDEATHSIG`; the notice of a parent's exit is
      a kqueue `EVFILT_PROC` filter with `NOTE_EXIT`. Nothing run in `pre_exec`
      outlasts the `exec`, so where that watch runs (for instance a small
      wrapper process that starts the daemon and signals it when the parent
      goes) is this task's to settle and record. Done when: a runner test kills
      the parent with `SIGKILL` and sees the daemon gone within a second. — done
      18c8dee, green on the macOS runner (run 36640376030)
- [~] **M2.31** Take `-A dead_code` off the macos job's clippy line in
      `.github/workflows/ci.yml` (S1.53). It is there because df-app items
      whose only callers are Linux bodies — the drag-and-drop helpers in
      `dnd.rs`, gio's output readers in `mounts.rs`, `platform/icon.rs`,
      `platform::desktop::Event`'s drag and clipboard variants and others the
      Linux device alone constructs — are dead on macOS until this phase gives
      them callers (M2.10–M2.15). Done when: the last of those lands and the
      macos job's clippy passes without the flag.
      — blocked: with M2.10–M2.15 and M2.12 in, the macOS build has 29 dead items
      left (and 5 more in the test build), down from 49, and none of them waits on a
      Phase 2 task: they are Linux's alone — gio's listing and monitor readers and
      `Spec`'s helpers in `mounts.rs`, the `Event`/`Change` variants and
      `Reply::Mounted` and `Connected::NeedsTerminal` only gio makes,
      `dnd::{wanted_mime, paths_from, is_ours}` which only the Wayland device reads,
      `PasteFailure::{Stalled, Broken}`, `Link::Starting` and `ClipError::Missing`.
      The flag stays. Options: (a) move those items into `platform/linux/` with
      their tests (a move under the Linux-unchanged rule, most of it `mounts.rs`);
      (b) mark each `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]`, which
      needs an exception to "no cfg outside platform/"; (c) keep `-A dead_code` on
      macOS for good, with the linux job, where every item has its caller, as the
      one that holds dead code out.

## 9. Found in Phase 2

- [x] **M2.32** `cargo test -p df-core` green on the macOS runner (added
      2026-09-29: the first CI run had 9 failures there). The rsync tests skip
      where rsync is older than 3.1 (M2.6); the zip test lists the UTF-8 name
      with bsdtar where Apple's `unzip` prints bytes 0x80–0x9F as `?`, and
      takes libarchive's NFD spelling of it; a socket path is measured against
      the platform's `sun_path` (`platform::socket::PATH_MAX`: 107 on Linux,
      103 on macOS) and the tests that bind one do it under `/tmp` when
      `$TMPDIR` is too deep; the fake server that closes at once is `true` on
      `PATH`, not `/bin/true`. Done when: the macos job's `cargo test -p
      df-core` step passes. — done 0ad983a, green on the macOS runner
      (run 36640376030)
- [x] **M2.33** `platform::xattr` macOS body (added 2026-09-29, when Brian
      delegated the open question on tags): tags are Finder's
      `com.apple.metadata:_kMDItemUserTags`, a binary plist of names read and
      written by a hand-written codec limited to an array of strings
      (`platform/macos/bplist.rs`), each name with an optional `\n<colour
      index>`; `fs::tags`' `user.xdg.tags` is answered from it (names joined
      with commas), the seven colours written with Finder's indices (grey 1,
      green 2, purple 3, blue 4, yellow 5, red 6, orange 7) and a colour Finder
      gave a tag kept when the tags are written again; other `user.*`
      attributes are carried with `getxattr`/`setxattr`/`listxattr`/
      `removexattr` and `XATTR_NOFOLLOW`; a tag on a link is refused as on
      Linux. Done when: the tag tests in `fs/tags.rs` and the body's own run
      on the macOS runner, a tag written there reads back from Finder's
      attribute as Finder's plist, and live check V7 §4.4. — done ecf89d6,
      green on the macOS runner (run 36640376030)
- [x] **M2.34** df-app builds, lints and passes its tests on the macOS runner
      with the stubs as they are, before any Phase 2 body lands. Run
      36628614462 on `main` was the first to build df-app for macOS: the
      build, clippy (with `-A dead_code`, S1.53) and `--version` passed as
      the code stood, and `cargo test -p df-app` had 8 failures of 1,102.
      Fixed as portability bugs: `app::syncing::tests::
      a_socket_is_left_out_in_the_toast_and_is_no_problem` bound its socket
      deep inside the temporary tree, past `sun_path`'s 104 bytes under
      macOS's `/var/folders/…/T/` — it now binds at a short path and renames
      the socket into place; `app::tests::hits::
      making_something_in_the_hits_is_refused` asserted that `d` is live in a
      search's hits, which a platform with no trash yet refuses everywhere —
      the trash line now expects what `Trash::home()` implies. Gated
      `#[cfg(target_os = "linux")]`, because their fixtures are the
      freedesktop trash's `<trash>/files/<name>` layout, which is Linux's
      alone: `trashview::tests::a_trash_record_becomes_a_list_pane_row`,
      `trashview::tests::a_restore_is_refused_rather_than_overwriting_newer_work`,
      `trashview::tests::rows_map_back_to_the_records_they_came_from`. Not
      df-app's to fix, and left failing: `app::syncing::tests::
      y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync` (the runner's
      `/usr/bin/rsync` is openrsync, which `df_core::sync::rsync::available()`
      accepts until df-core's M2.6 version gate lands, after which the test
      skips), and `icons::glyph_tests::the_chrome_glyphs_all_render` and
      `menu::tests::a_key_stands_clear_of_the_chevron` (no Nerd Font on the
      runner, as on the linux job; the chrome-glyph work fixes them on every
      target). Done when: on the macos job, df-app builds, its clippy line
      passes, `--version` runs, and every df-app test passes except those
      three.
      — done ccb657f; run 36636630419 on this branch: df-app built, linted and ran
      `--version` on the macOS runner, and 1,128 of its 1,129 tests passed, the one
      left the rsync test above.
- [ ] **M2.35** Emptying old trash on macOS (`[mgr] trash_keep_days`, added
      2026-09-29 on the df-core branch as M2.34, renumbered at integration): `purge_expired`, `purge_expired_if_due` and `purge_due_in`
      over the M2.8 journal, with a stamp beside it so one window a day purges,
      as Linux's does over the freedesktop trash. Until then `purge_due_in` is
      never due and the other two refuse ("Emptying old trash is not available
      on this platform"). Done when: the aging tests of
      `platform/linux/trash.rs` have macOS twins over the journal that pass on
      the runner.

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
- (df-app) 2026-09-29 — M2.30 appended: light mode's `auto` (which arrived after
  this plan) reads the XDG portal on Linux; on macOS S1.35 leaves a stub that
  answers nothing, and M2.30 is its native body.
- 2026-09-29 — The appearance body was appended as `M2.28` on the df-app branch
  while the df-core branch took `M2.28` for the chmod walk; at integration the
  chmod walk kept it and the appearance body became `M2.30`
  (`01-platform-seam.md` Decisions log).
- (df-app) 2026-09-29 — M2.34 appended: the first macOS build of df-app, on
  `main`'s run 36628614462, passed its build, clippy and `--version` but not
  its tests, and a task was owed for making them hold before any body landed.
- (df-app) 2026-09-29 — M2.34: the three `trashview` tests whose fixtures are
  the freedesktop layout are `#[cfg(target_os = "linux")]`; the hits test's
  trash line follows `Trash::home()` rather than being gated, so the rest of
  that long test still runs on macOS.
- (df-app) 2026-09-29 — Local type checks of df-app for macOS: see
  `00-ground-rules.md` §6. Every objc2 call in this phase was compiled on Linux
  before it met the runner.
- (df-app) 2026-09-29 — M2.10: a list of files is read back item by item
  (`pasteboardItems` → `public.file-url` → `NSURL` → `filePathURL` → `path`),
  not with `readObjectsForClasses:`, whose class array objc2-app-kit 0.2 types
  as `NSArray<AnyObject>`; Finder's file reference URLs resolve the same way.
  The paste table takes `com.compuserve.gif` and `org.webmproject.webp` too, so
  what the copy table writes can be pasted back. The runner tests use a
  pasteboard of their own, from the same server, so a developer running the
  tests keeps their clipboard.
- (df-app) 2026-09-29 — M2.13: the mirror of the pasteboard's types is taken
  when `changeCount` has moved, and the count is read at most every 200 ms,
  since each read is a round trip to the pasteboard server and a frame can
  come every few milliseconds. A paste reads the bytes themselves, so a
  mirror a fifth of a second old picks the type and nothing else.
- (df-app) 2026-09-29 — M2.14: an ejectable disk's `drive` is its mount
  point, not `None` as planned: the card's `e` refuses a disk with no drive,
  and macOS ejects by where the volume is mounted. A share's `url` is
  `NSURLVolumeURLForRemountingKey` (`smb://host/share`) where the system has
  one and `statfs`'s `f_mntfromname` (`//user@host/share`) where it does not,
  with the scheme then taken from the filesystem type (`smbfs` → `smb`).
- (df-app) 2026-09-29 — M2.15: `dav://` and `davs://` are refused as `sftp://`
  is: Finder mounts WebDAV only from its own Connect to Server, with an
  `http(s)://` address that `openURL` would open in a browser. A connect waits
  for any new entry under `/Volumes`, as planned, rather than matching the
  share by address: the address macOS records for a share carries a user and a
  Bonjour name the typed address need not.
- (df-app) 2026-09-29 — M2.16: the child is collected by a thread of its own
  once it exits (`platform::open::release`), since nothing forks in between
  and a launched editor closed an hour later would otherwise stay a zombie for
  as long as delightfile ran. The shared Unix spawn calls
  `platform::open::{detach, release}`; on Linux both do nothing, and `setsid
  --fork` in the argv is the detaching as before.
- (df-app) 2026-09-29 — M2.12: each file's pasteboard item carries its path as
  `public.utf8-plain-text` beside its URL, in place of one extra item holding
  every path: Finder makes a text clipping out of an item with only text, and
  a drag of two files onto the Desktop would leave a third thing there. The
  window's mark (`dnd::self_mime()`) rides on the first item as a type with no
  bytes. The session offers only `Copy`: nothing leaves the window as a move.
  The picture is drawn at one pixel to the point, as for Wayland at scale 1,
  and AppKit scales it on a Retina screen.
- (df-app) 2026-09-29 — M2.26: `exiting` wraps up a quit that did not come
  through `finish` as a closed window would (`WriteCwd`). On Linux every quit
  comes through `finish` first, so nothing changes there; the one Linux path
  that could reach `exiting` without it, an event loop that ends on its own,
  now writes the cwd file too, which is what closing the window does.
- (df-app) 2026-09-29 — M2.30: the `NSApp.effectiveAppearance` route shipped,
  with a key-value observer class of our own registered with
  `NSKeyValueObservingOptionInitial`, so the first answer is in before
  `watch_over` returns. On macOS `appearance::Connect` is a unit struct: there
  is no connection to make, only the application to ask. A watcher started off
  the main thread (a test's) is `Link::Gone` at once.
- (df-app) 2026-09-29 — M2.9: on macOS the line under an empty trash view is
  what the view cannot see, in place of the trash clock's line; the clock is
  not lost, since its purge says what it removed in its own toast.
- (df-app) 2026-09-29 — M2.17, M2.21 and 05's D5.3, D5.5 and D5.11 are
  blocked rather than done in df-core from this branch: it was asked to keep
  out of df-core while `port/macos-core` and `port/paths` change it.
- (df-core) 2026-09-29 — Tags on macOS are Finder's tags
  (`com.apple.metadata:_kMDItemUserTags`), so a tag set in delightfile shows
  in Finder and the reverse. Brian delegated the open question on 2026-09-29;
  it is removed below and M2.33 appended for the body.
- (df-core) 2026-09-29 — M2.1: a kqueue watch is on the directory's vnode, so
  it hears names made, removed and renamed in it, and not a file inside
  rewritten in place or `chmod`ed; a row's size or date is then brought up to
  date by the next rescan. A save through a temporary file and a rename is
  seen. There is no `Overflow` on macOS (`EV_CLEAR` folds, never drops). A
  burst is held to one refresh per debounce window it spans: the runner took
  seven windows to write the 200-file burst the Linux test holds to four, so
  both watcher tests bound it by the time the burst took, with four as the
  floor.
- (df-core) 2026-09-29 — M2.2: `clone_before_open` takes the reader the copy
  already has open, not the source's path (the task sketched `(src, dst)`),
  so the clone is of the file the copy opened. `ENOTSUP` and `EOPNOTSUPP`
  (different numbers on macOS), `EXDEV` and `EINVAL` fall back to the chunked
  copy. The clone is opened read-only for the durable flush, since a
  read-only source's clone cannot be opened for writing. `without_reflink`
  turns it off in tests as it does `FICLONE`.
- (df-core) 2026-09-29 — M2.3: `fs::tags::read_here` asks `du::is_remote`
  instead of matching `du::magic_of` against the Linux magics itself — the
  same answer on Linux, where `is_remote` is that match, and on macOS the one
  that knows a share by its `statfs` type name, so tags are not read per row
  on an SMB share.
- (df-core) 2026-09-29 — M2.4: each uid and gid is asked of Open Directory
  once per process and the answer kept, a name or none; names are leaked for
  the process so the call answers `&'static str` as Linux's does. The owner
  test's "unknown" id became 3999999999: 4294967294 is macOS's `nobody`.
- (df-core) 2026-09-29 — M2.5: libc 0.2.189 has
  `pthread_set_qos_class_self_np` and `qos_class_t` for Apple after all, so
  they are used from libc; the task text is corrected. Every nice level is
  `QOS_CLASS_UTILITY`.
- (df-core) 2026-09-29 — M2.6, a Linux-visible change: `rsync::available()`
  requires 3.1.0 or newer on every Unix, read from `rsync --version` with
  `LC_ALL=C`. On Linux that refuses only an rsync old enough that every run
  already failed on `--info=progress2`; `parse_version` also takes a `v`
  before the numbers. `RSYNC_HINT` is `""` on Linux and Windows, so Linux's
  sentence is unchanged.
- (df-core) 2026-09-29 — M2.8: `TrashedItem` keeps the four fields every
  target and df-app share. On macOS `trash_root` is the journal (the Trash's
  root, so a redo's `Trash::at(item.trash_root)` is the same trash) and
  `location()` reads the item's line back from it, cached per journal by its
  size and date — the task's sketch gave the item a `location` field, which
  would change the struct on every target and df-app's literals of it. The
  journal is `state_dir()/delightfile/trash-journal`, beside the `state`
  file (the task's `state_dir()/trash-journal` read as delightfile's own
  state directory), changed only under an exclusive lock on
  `trash-journal.lock`; `files_dir()` is the journal, as the stub's root was;
  `info_path()` is the location. A purge drops the line first, as Linux drops
  the `.trashinfo` first. Tests trash into the runner's real `~/.Trash`
  (NSFileManager chooses) with a journal in their fixture, and purge what
  they leave. Aging is M2.35.
- (df-core) 2026-09-29 — M2.18: yazi's `yazi-fs/src/xdg.rs` has no macOS
  branch (config `~/.config/yazi`, state `~/.local/state/yazi`, temp
  `std::env::temp_dir()` + `yazi-<uid>`), so macOS keeps the XDG rules and
  the thumbnail cache is shared as on Linux. `runtime_dir()` is `$TMPDIR`;
  the Unix `$XDG_RUNTIME_DIR` body is Linux's alone.
- (df-core) 2026-09-29 — M2.28: without `O_PATH` a descriptor needs read
  permission, so a folder its owner has shut is found and opened when next
  used (its inode checked then), and until then refuses every use with
  `EACCES` — what Linux's `O_PATH` descriptor answers — which keeps the
  "Apply again" sentence. The walk's types are the platform's
  (`nofollow::{Folder, Entry, Stat, meta}`; on Linux `File`, `File`,
  `Metadata` and `platform::meta`), since `std::fs::Metadata` cannot be made
  from an `fstatat`; `ops/mode.rs` changed in its type names only. The last
  name is looked up once by `fstatat` and once by `fchmodat`, so a file
  renamed over it between the two is caught by the inode the record keeps
  rather than by a descriptor. `fchmodat` with `AT_SYMLINK_NOFOLLOW` sets a
  link's own mode rather than refusing it, as the task text had it; it never
  follows one, and the lookup before it has already left links alone. The
  task text is corrected.
- (df-core) 2026-09-29 — M2.29: the watch runs in a process of its own,
  forked by the daemon-to-be between `fork` and `exec` (the daemon's child,
  never exec'd): it closes every inherited descriptor — `std`'s exec-error
  pipe among them, which would otherwise keep `spawn` waiting — and waits in
  kqueue on `EVFILT_PROC`/`NOTE_EXIT` for delightfile or the daemon, sending
  the daemon `SIGTERM` if delightfile goes first. A thread's exit is no
  kqueue event, so on macOS the daemon is tied to the process, not the
  thread. It shows as a second `delightfile`, a child of `rclone`.
- (df-core) 2026-09-29 — M2.32: the right directory for a real rclone socket
  on macOS is `$TMPDIR/delightfile/` (the runtime directory, M2.18), then
  `$TMPDIR/delightfile-<uid>/`, then `/tmp/delightfile-<uid>/` when a long
  remote name passes `sun_path`'s 103 bytes; the bound is now the platform's
  (`size_of::<sockaddr_un>() − offset_of!(sun_path) − 1`), not Linux's 107.
- (df-core) 2026-09-29 — M2.33: a comma list is the seam `fs::tags` speaks,
  and the macOS body translates it: reading takes the `\n<index>` off each
  name, writing gives each name the colour Finder already had for it on the
  file, else a colour tag's own index, else none, so a clone keeps every
  colour and a chunked copy keeps the colour tags'. Finder's attribute is
  listed as `user.xdg.tags`, and a literal `user.xdg.tags` (a disk tagged on
  Linux) is hidden behind it and not read. A link could hold attributes on
  macOS but is refused a tag, as on Linux. A Finder tag with a comma in its
  name reads as two.
- (df-core) 2026-09-29 — Stubs only Windows still stands behind are compiled
  only there (`#[cfg(windows)]` on their lines in `platform/stub/mod.rs`),
  so none is dead code on macOS; every df-core stub is now Windows-only.
- (df-core) 2026-09-29 — M2.32, M2.33 and M2.34 were numbered on the df-core
  branch while the df-app branch works in parallel; if the two branches took
  the same numbers, the df-core ones keep M2.32 (named in the brief) and
  M2.33 (the tags decision), and whichever is merged second renumbers
  anything else.
- 2026-09-29 — Integration of `port/macos-core` onto `main` (on
  `port/macos-finish`), Brian's call: both branches had taken M2.34. `main`'s
  (df-app builds and passes on the macOS runner) keeps it; the df-core
  branch's (emptying old trash from the journal) is **M2.35**, here and in
  df-core's comments. M2.32 and M2.33 are the df-core branch's and stand. The
  df-core commits were cherry-picked one for one with their messages, so
  every `done <sha>` above names the commit on `port/macos-finish`; the
  df-app branch's shas, which named its commits before they were rebased
  onto `main`, name `main`'s. Section 9 holds every task found in Phase 2,
  both branches' in number order.

## Open questions

- Whether `NSApp.currentEvent` inside winit's `CursorMoved` dispatch is the
  `mouseDragged` event AppKit requires for `beginDraggingSession` (M2.12). If not,
  the fallback is an `NSView` subclass override of `mouseDragged:` — record which.
  (df-app, 2026-09-29: M2.12 is built on the first; only the live check in
  `07-verification.md` §4.5 can answer.)
- (df-app) What a connect that saw nothing appear under `/Volumes` within ten
  seconds should say (M2.15). The plan's `Mounted(None)` makes the window say
  "Connected to …", which is untrue while Finder's dialog is still asking for a
  password and true when the share was mounted already. Options: keep it; a
  new `Connected` variant the window words as "Finder is connecting to … —
  `M` lists the share once it is mounted"; or wait longer.
- (df-app) Who makes the df-core halves of M2.17 and M2.21 (and 05's D5.3,
  D5.5, D5.11): the df-core branch, a pass after the port branches merge, or
  this branch with leave to touch df-core. The options are in each task.
- (df-app) M2.31: how the items that only Linux calls stop being dead code on
  macOS, if they should: moved into `platform/linux/`, allowed per item with a
  `cfg_attr`, or `-A dead_code` kept on macOS for good. The options are in the
  task.
- Ghostty on macOS: does `ghostty -e` work from `open -a`? Affects the `edit` opener
  default in `05-defaults-and-config.md`.
