# 02 — macOS

Status: **done** at "compiles, unit tests pass on the macOS runner" (2026-09-29,
`port/macos-finish`): every task is `[x]` or `[~]` with its reason. Nothing
here has been seen on a Mac's screen; that pass is `07-verification.md` §4.
One task has been added since and is open: M2.36, Open With from Finder
(2026-09-30).

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
      target_os = "macos"))]` and pass on the macOS runner. — done 5730281, green on
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
      asserts the copy took under 50 ms; skipped on other targets. — done 094f466,
      green on the macOS runner (run 36640376030)
- [x] **M2.3** `platform::fs::is_remote`: `statfs` and `f_fstypename` in
      `{"nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp"}` or starting with
      `"fuse"`/`"macfuse"`/`"osxfuse"`. `magic_of` returns `f_type` for parity.
      Done when: a unit test with a fake `statfs` result table passes. — done
      1ed8bc4, green on the macOS runner (run 36640376030)
- [x] **M2.4** `platform::user::owner_names`: macOS body uses `getpwuid_r`/`getgrgid_r`
      (Open Directory serves them; `/etc/passwd` lists only system accounts). The
      Linux body keeps its `/etc/passwd` parser. Done when: `fs::owner` tests pass on
      both. — done 358ccb4, green on the macOS runner (run 36640376030)
- [x] **M2.5** `platform::thread::lower_priority`: macOS body
      `pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0)` for `nice > 0`, leaving
      the calling thread's class at UTILITY. Both are in libc 0.2.189
      (`libc::pthread_set_qos_class_self_np`, `libc::qos_class_t::QOS_CLASS_UTILITY`,
      re-exported from its `new/apple/libpthread`), so nothing is declared
      locally. Done when: compiles and a test asserts the call returns 0. — done
      2e98de6, green on the macOS runner (run 36640376030)
- [x] **M2.6** `platform::process::rsync_available()` gates on version: parse
      `rsync --version`'s first line and require ≥ 3.1.0 (`--info=progress2`);
      `platform::process::RSYNC_HINT` = "rsync 3.1 or newer — `brew install
      rsync`", which the refusal in `app/syncing.rs` names as what is needed in
      place of the bare "rsync" ("Sync to a server needs rsync 3.1 or newer —
      `brew install rsync`"); `""` on Linux and Windows, where the sentence
      stays "Sync to a server needs rsync". Done when: the
      pure parser test covers `rsync  version 2.6.9`, `3.2.7` and openrsync's
      banner (returns false). — done b101fa7, green on the macOS runner (run
      36640376030); the df-app half is the df-app branch's — done 89def0e on
      `port/macos-finish` and made to read once in c74fbc5 (Brian's call:
      the first composition said "needs rsync" twice), compiles and
      unit-tested on the macOS runner, not seen on screen: `needs_rsync`
      names the hint in the bare "rsync"'s place, and Linux's sentence is as
      it was (`app::syncing::tests::a_sync_with_no_rsync_says_what_the_platform_needs`);
      the window's rsync test skips where no rsync is 3.1 or newer. Live check
      07-verification.md §4.7.
- [x] **M2.7** `platform::fs::forget_cached`: `fcntl(fd, F_NOCACHE, 1)` is *not*
      used (it changes the file's caching mode, not a hint); the macOS body stays a
      no-op as in S1.5. Done when: recorded here; nothing to do. — done 1ed8bc4,
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
      is empty again. — done 57176bb, green on the macOS runner (run 36640376030)
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
      (Decisions log). What `Mounted(None)` says, which was an Open question,
      Brian settled: a connect that saw no share appear says "Finder was asked
      to connect to …", not that it connected — done 761fc15 on
      `port/macos-finish`, green on the macOS runner (run 36649716767), not seen on
      screen: the words are `platform::mounts::CONNECT_UNSEEN` (`None` on
      Linux and Windows, so Linux still says "Connected to"), and
      `app::tests::a_connect_with_no_share_to_go_to_says_what_is_known` reads
      both. Live check §4.7.

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
- [x] **M2.17** The macOS default opener and rule tables from `05-defaults-and-config.md`
      §2 are wired into `platform::defaults::{OPENERS, RULES, BOOKMARKS}` and
      `df_core::config::Config::default` reads them. Done when: `open.rs`'s
      `opener_rules_pick_by_glob_then_mime` has a macOS twin asserting `open "$1"`.
      — (the df-app branch had it blocked on df-core; `port/macos-finish` was
      given leave to make the df-core half) done 29cfb82 (with D5.3 and D5.11),
      green on the macOS runner (run 36649716767), not seen on screen.
      `df_core::platform::defaults` has the three tables per target: Linux's
      are `config.rs`'s constants re-exported, unchanged; Windows ships
      Linux's openers and rules until W4.3 and §6's bookmarks of its own; and
      macOS's are 05 §2.1's openers, §2.3's rules — Linux's row for row with
      `open` where delightviewer was (ebb95b8, Brian's call over the literal
      reading first shipped) — and §6's bookmarks, `g D` and `g o` among them
      (f0d0573). `open.rs`'s `opener_rules_pick_by_glob_then_mime_on_macos`
      finds `open "$1"` first for a picture, a PDF and an unknown file; the
      Linux-table tests run where those tables ship (Decisions log). Live
      check §4.7.
- [x] **M2.18** `platform::dirs` macOS values per `05-defaults-and-config.md` §1
      (D5.1 lands them; this task is the cross-reference). Done when: D5.1 done. —
      done da3fdb1, green on the macOS runner (run 36640376030); the macOS column of
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
- [x] **M2.21** Labels: `df_core::keymap::Chord::label` (`keymap/key.rs:323–345`)
      takes a `df_core::platform::keys::LABELS` table (pure strings, in df-core —
      df-core has no winit, so this is a separate module from df-app's
      `platform::keys::mods`): macOS `⌃`/`⌥`/`⇧`/`⌘` with `Ctrl+`
      rendered as `⌘` (since Cmd is the primary), Linux/Windows unchanged. The six
      hard-coded UI strings (appendix B §3: `cli.rs:170`, `overlay.rs:549`,
      `app.rs:12393, 12845, 16480, 16781`) go through `df_core::platform::keys::ctrl_name()`
      ("Ctrl" / "⌘"). Done when: `keys.rs:164`'s `"Alt+←"` assertion has a macOS
      twin `"⌥←"`.
      — (blocked on df-core on the df-app branch; option (a) taken) done b6866c9
      on `port/macos-finish`, with three window tests that read a label as text
      taught a Mac's in 619da12 (run 36648279848 found them), green on the
      macOS runner (run 36649716767), not seen on screen. `df_core::platform::keys::LABELS` is each modifier's mark in the
      order a label writes them: `Ctrl+ Alt+ Super+ Shift+` on Linux and
      Windows, so every Linux label is what it was, and `⌃ ⌥ ⇧ ⌘` run together
      in Apple's order on macOS, the `ctrl` role wearing `⌘`
      (`keys::tests::modifiers_carry_through` asserts `"⌥←"` there). The
      strings that named a chord by hand — the search's "Ctrl+s to stop" and
      its `Ctrl+s` and `Alt+Enter` hints, "Ctrl+N opens another window",
      "press Ctrl+v again", `--help`'s `Ctrl+Enter` — are written through the
      chord label (df-app's `keys::written`), and "Ctrl-click selects"
      through `ctrl_name()`; Linux's `--help` is byte for byte what it was.
      `super+…` in a Mac's `keymap.toml` binds the Cmd chord and warns "super
      is Cmd, which is Ctrl on macOS" (05 §3). `⌥` and `⌃`, which neither
      stock face draws (`⌘` is in one), are stand-ins in `glyphs.rs` beside
      `⇧`. Live check §4.2.
- [x] **M2.22** Pointer modifiers: `app.rs:13785–13787` already treats egui's
      `command` (Cmd) as `toggle`; confirm `dnd::verb_for` gets Cmd → Copy and
      Option → Link on macOS and add the mapping to the help sheet's mouse section.
      Done when: reviewed; live check.
      — done, nothing to commit (reviewed 2026-09-29). `pointer.toggle` is egui's
      `command || ctrl`, and egui-winit sets `command` from Cmd on macOS, so
      Cmd-click toggles and Cmd-drag is `Verb::Copy`; Option is egui's `alt`, so
      Option-drag is `Verb::Link`. The help sheet has no mouse section to add the
      mapping to; its "⌘-click toggles, ⌥-drag links" line is D5.5's, and
      Brian left it out: it is shown nowhere (05 D5.5). Live check §4.5.
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
      — skipped: optional, and waiting on the live check it names (§4.6);
      software decode stands until a Mac shows 4K playing badly.

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
      2198cc9, green on the macOS runner (run 36640376030)
- [x] **M2.29** `platform::process::tie_to_this_thread` macOS body (S1.50), in
      place of the no-op under which an rclone daemon outlives a delightfile that
      crashes. macOS has no `PR_SET_PDEATHSIG`; the notice of a parent's exit is
      a kqueue `EVFILT_PROC` filter with `NOTE_EXIT`. Nothing run in `pre_exec`
      outlasts the `exec`, so where that watch runs (for instance a small
      wrapper process that starts the daemon and signals it when the parent
      goes) is this task's to settle and record. Done when: a runner test kills
      the parent with `SIGKILL` and sees the daemon gone within a second. — done
      7793732, green on the macOS runner (run 36640376030)
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
      — skipped in part, as Brian decided (a) for the functions and (c) for the
      variants: the Linux-only functions and their tests moved, line for line,
      in a3fb809 on `port/macos-finish` — gio's readers from `mounts.rs` to
      `platform/linux/gio.rs`, the Wayland device's drop readers from `dnd.rs`
      to `platform/linux/wayland/incoming.rs` — and the macOS build (df-core,
      the dv crates and df-app, lib and tests) now leaves six enum variants
      unused and nothing else. No task gives any of them a maker on macOS,
      because each is how a Linux body says something macOS says another way:
      `ClipError::Missing` (no `wl-copy`; the pasteboard is always there, and
      Windows' stub makes it), `mounts::Reply::Mounted` (udisks2's mount; macOS
      mounts disks itself, M2.14), `mounts::Connected::NeedsTerminal` (gio's
      password prompt; Finder asks in its own dialog, M2.15),
      `desktop::PasteFailure::{Stalled, Broken}` (the Wayland data device's
      pipe; the pasteboard is synchronous, M2.13), `appearance::Link::Starting`
      (the portal's connection; macOS answers before `watch_over` returns,
      M2.30), and `mounts::Change::{VolumeAdded, VolumeRemoved, MountAdded,
      MountRemoved, Other}` (`gio mount --monitor`'s events; macOS has no
      watcher, and one that heard `NSWorkspace`'s mount notices would be a new
      task). So the macos job keeps `-A dead_code`, and its comment in
      `ci.yml` names the six.

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
      df-core` step passes. — done 3ff30a7, green on the macOS runner
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
      attribute as Finder's plist, and live check V7 §4.4. — done 826df6a,
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
      — done ccb657f; run 36636630419 on the df-app branch: df-app built, linted
      and ran `--version` on the macOS runner, and 1,128 of its 1,129 tests
      passed, the one left the rsync test above. Since M2.6 and M2.8 are in
      (`port/macos-finish`), the rsync test skips there, the three `trashview`
      tests have macOS twins built through the journal (e9be774), the
      trash-refusal test is Windows' alone (cfcfc8b), and the hits' undo and
      redo of a trash run on every Unix, checked through the trash's own
      list (abdfa65). `app/tests/trash.rs` stays Linux's (Open questions).
- [x] **M2.35** Emptying old trash on macOS (`[mgr] trash_keep_days`, added
      2026-09-29 on the df-core branch as M2.34, renumbered at integration):
      `purge_expired`, `purge_expired_if_due` and `purge_due_in`
      over the M2.8 journal, with a stamp beside it so one window a day purges,
      as Linux's does over the freedesktop trash. Until then `purge_due_in` is
      never due and the other two refuse ("Emptying old trash is not available
      on this platform"). Done when: the aging tests of
      `platform/linux/trash.rs` have macOS twins over the journal that pass on
      the runner.
      — done eae9889 on `port/macos-finish`, green on the macOS runner (run
      36645084634, and 36649716767), not seen on screen. Only the journal's lines are
      ever chosen, so nothing Finder or anything else put in the Trash is
      touched; a line whose date is the keep old goes, with no zone slack
      (Decisions log). The stamp is `trash-journal.purge` beside the journal,
      `flock`ed for the run and written only when it completes. The eight
      aging tests of `platform/linux/trash.rs` have twins in
      `platform/macos/trash.rs`, over a journal and a Trash folder of the
      test's own. Live check 07-verification.md §4.4.
- [ ] **M2.36** Open With from Finder (added 2026-09-30, from
      `06-build-and-release.md`): a folder chosen in Finder's Open With, or
      dropped on the Dock icon, reaches the app as an open-documents event,
      `application:openURLs:` on the application delegate, which winit 0.30
      does not pass on, so the app would start in its usual folder. Hook
      winit's `NSApplicationDelegate` with objc2 (0.5, `00-ground-rules.md` §3)
      in `platform/macos/` so the URLs reach the app, and open each the way
      `delightfile <path>` does; then put `CFBundleDocumentTypes` back into
      `build/macos/Info.plist` (`public.folder`, role Viewer, rank Alternate),
      which came out on 2026-09-30 until this lands. Done when: a unit test of
      the URL-to-path step passes on the macOS runner, the declaration is back
      and `plutil -lint` passes, and live check `07-verification.md` §4.7
      "Open With from Finder" passes.

## 10. Native feel

- [x] **M2.37** The window's own top row in the title bar, and the app menu
      in the menu bar (Brian, 2026-09-30), as Finder and Safari are one
      header with their toolbars and every Mac program keeps its menu at the
      top of the screen. The model is Windows' W4.39 (`04-windows.md` §12);
      Linux and Windows do not change.
      - The title bar (`platform/macos/window.rs`): the window is made with
        `with_titlebar_transparent(true)`, `with_fullsize_content_view(true)`
        and `with_title_hidden(true)` beside `with_option_as_alt`, so the top
        row is drawn at the window's top with the traffic lights over its
        left end. `window::title_band` answers a band whose `left_inset` is
        as far in as the rightmost of AppKit's three buttons reaches
        (`standardWindowButton` frames, in the window's coordinates; 78
        points when AppKit will not say), whose `right_inset` is 0 and whose
        `height` is the lights' foot, which the row is deeper than, so the
        band is the row's; `None` in full screen. The layout needs nothing
        new: with one tab the top row is in the band and starts a gap past
        the lights, with two or more the strip does, the top row under it
        the window's width.
      - The lights sit level with the row (Brian, 2026-09-30, after the
        first build): after every layout `window::band_row` is given the row
        in the band, its first line (`ui::Layout::band_row`), and AppKit's
        title bar — the close button's superview's superview — is made
        twice as deep as the row's middle is low, its top still the
        window's, with each button centred in it (`caption::lights_at`), as
        Electron's `trafficLightPosition` does. Written only when the
        buttons are not already there; put back where AppKit had them when
        the band goes (full screen).
      - The band is still a title bar. After every layout
        `window::title_regions` keeps the band and the window's controls in
        it (`chrome::band_controls`, as on Windows); a new seam function,
        `window::title_press`, is asked in the window's `MouseInput` arm
        before egui sees a primary press, inside the `mouseDown:` it came in,
        and a press in the band on none of the controls
        (`caption::classify`, with no resize edge and no buttons) is handed
        to `winit::Window::drag_window()` (AppKit's
        `performWindowDragWithEvent:` with that press), or, the second of a
        double click, sends `performZoom:` or `performMiniaturize:` as
        `AppleActionOnDoubleClick` says (`caption::double_click`). The
        window never sees such a press. Linux's and Windows' bodies say no
        press is the title bar's.
      - The menu bar (`platform/mirror.rs`, pure; `platform/macos/menubar.rs`,
        AppKit): the window builds the app menu's rows as the ☰ button would
        open them (`App::app_menu_items`) at the end of every frame and
        `menubar::publish`es them; `mirror::bar` lays them under File, Edit,
        View, Go and Help (the mapping is in the Decisions log), beside the
        application menu (About delightfile, Services, Hide delightfile ⌘H,
        Hide Others ⌥⌘H, Show All, Quit delightfile ⌘Q, which is the ☰
        menu's Quit and sends `terminate:`, M2.26's path) and Window
        (Minimize ⌘M, Zoom, Bring All to Front, and AppKit's list of
        windows). No Preferences…: the app has no settings command. Each
        mirrored menu is built from the latest rows by its delegate's
        `menuNeedsUpdate:` as it opens; a row shows its key in AppKit's
        notation when the ☰ row's key is one chord (Cmd is the keymap's
        `ctrl`, M2.20), is grey where the ☰ row is grey and ticked where it
        is ticked. Choosing a row sends `delightfileChoose:` to a
        `declare_class!` target, which queues the row's action and rings the
        window's bell; the window `menubar::take`s the queue after the keys
        in its next frame and does each through `App::menu_action`, the ☰
        menu's own door.
      - The ☰ button is gone from macOS's top row
        (`menubar::MENU_BUTTON`, `false` on macOS alone): the breadcrumb
        starts at the row's corner, and `F10` still opens the app menu in
        the window, from that corner.
      Done when: the mapping's tests (`platform::mirror`: every ☰ row has a
      home, once; the groups where the Decisions log says; greys and ticks
      carried; chords as key equivalents), the band's (`ui::tests::
      the_traffic_lights_are_kept_clear_at_the_bands_left_end`,
      `caption::tests::with_no_edge_and_no_buttons_only_the_controls_are_the_windows`,
      the double-click setting), Linux's pinned layouts and ☰ button, and on
      the runner the menus built and read back
      (`platform::macos::menubar::tests`), pass; live check
      `07-verification.md` §4.1, the lines marked M2.37.
      — done 73f29f6, fdf7c2c, 51b1139, green on the macOS runner (run
      36742883643: df-app 1,217 there, the menus built and read back among
      them; Linux and Windows green in the same run, Windows on a second
      attempt after df-core's
      `git::tests::the_cache_fills_in_asynchronously_and_bumps_the_generation`
      read its bell before the worker rang it, which this change does not
      touch), not seen on screen; the lights centred on the row in the
      commit after fe54200, which names this line, not seen on screen.
      `NSWindow.setMovableByWindowBackground(true)`, which the brief asked
      for beside `title_press`, is not set, by Brian's call once it was
      found to drag the whole window (Decisions log); the band drags through
      `title_press` alone.

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
- 2026-09-29 — Brian-delegated, and taken out of the Open questions below:
  the df-core halves of M2.17 and M2.21 (and 05's D5.3, D5.5 and D5.11) are
  made on `port/macos-finish`, which was given leave to edit df-core for
  them; a connect that saw no share appear says Finder was asked to connect
  to the address, not that it connected, and one whose share appears keeps
  "Connected to" (M2.15); and for M2.31 the Linux-only functions move into
  `platform/linux/` with their tests, moved and not rewritten, while enum
  variants and portable items a macOS caller does not exist for stay, with
  `-A dead_code` kept on the macos job and what remains listed.
- (macos-finish) 2026-09-29 — M2.6: the hint follows the refusal after a
  dash, as other toasts add a clause. The plan's own hint begins "needs
  rsync", so the Mac's sentence says it twice ("Sync to a server needs rsync
  — needs rsync 3.1 or newer — `brew install rsync`"); the constant is
  df-core's and was left as the plan wrote it. (Superseded by Brian's call
  below.)
- (macos-finish) 2026-09-29 — M2.15: the words are a platform constant,
  `platform::mounts::CONNECT_UNSEEN` (`Some` on macOS, `None` elsewhere), read
  when a connect comes back `Mounted(None)`, rather than a new `Connected`
  variant: only macOS would make one, and the linux job, which allows no dead
  code, would refuse a variant Linux never constructs.
- (macos-finish) 2026-09-29 — M2.17, D5.3, D5.11: Linux's tables stay the
  `DEFAULT_*` constants in `config.rs` (`DEFAULT_RULES` made `pub`), and
  `platform/linux/defaults.rs` re-exports them rather than holding a moved
  copy: the diff on Linux is the read going through the seam, and
  `port/paths`, which is changing `config.rs` now, meets three lines instead
  of two hundred. Windows re-exports the same tables until W4.3 writes its
  own (05 §2.2 needs that phase's argv splitter and `first_available`).
  (Windows' bookmarks are now its own: see Brian's calls below.)
- (macos-finish) 2026-09-29 — M2.17: 05 §2.3's "minus the rows that name a
  dropped opener" is read as written, whole rows. On a Mac a picture, a
  video, a song and a PDF therefore open in the default app through the
  fallback row; `edit-image` and `play` are in the opener table but in no
  rule; and a 3D model or a `.gcode`, which is `text/plain`, reaches the
  text rule and opens in Zed. The other readings are in Open questions.
  (Superseded by Brian's call below.)
- (macos-finish) 2026-09-29 — M2.21: the marks are written in Apple's order,
  `⌃⌥⇧⌘`, with nothing between them or the key, which is how macOS menus
  write a chord; the `ctrl` role is `⌘`, and `⌃` is the Super role's, which
  nothing makes on a Mac since M2.20 folds Super into `ctrl`. Chords named
  in running text go through the chord label (df-app's `keys::written`,
  which writes each such chord once and keeps it for the process, since
  the hint strip holds `&'static str`s), so they read as the keymap's own
  labels do; only "Ctrl-click" uses `ctrl_name()`. The search's
  `Alt+Enter` hint, which the plan's six strings did not name, is written
  the same way. The stock faces draw `⌘` and not `⌥` or `⌃` (checked with
  `drawn_by_a_face`), so those two became stand-ins. A Mac's labels are
  narrower (`⌘n` against `Ctrl+n`), so a menu's key column is too; the menu
  test that rolled the wheel "where the card was" now rolls it where the card
  was over the list, which on a Mac is only the card's scrollbar band.
- (macos-finish) 2026-09-29 — M2.31: moved to `platform::linux::gio`:
  `gio_mounts`, `shares_from`, `phones_from`, `first_line`, `Attempt`,
  `attempt`, `landing`, the monitor's `Blocks`, `event_from` and `listen`, the
  private helpers they use, and `Spec::{from_dir_name, same_server}` and
  `Protocol::of_monitor` as `impl` blocks there; to reach them, `Spec`'s
  fields, `decode` and `Address::segments` became `pub(crate)`. Moved to
  `platform::linux::wayland::incoming`: `dnd::{is_ours, wanted_mime,
  paths_from}`. Their fourteen tests moved with them unchanged, which takes
  two tests that also check the portable `Spec::of` and `dir_name`
  (`specs_are_written_and_read_the_way_gvfs_does`,
  `a_devices_spec_is_its_host`) off the macOS runner; `PIXEL` and
  `PIXEL_DIR` stay in `mounts`' tests, which the card's tests use too.
- (macos-finish) 2026-09-29 — M2.35: no zone slack. Linux's
  `DATE_SLACK_SECS` is there because the spec's `DeletionDate` is local time
  written by any desktop, and delightfile reads it as UTC; the journal is
  delightfile's alone and written in UTC, so a line exactly the keep old is
  old. `expired`, `purge_wait` and the stamp reading are the macOS module's
  own copies rather than shared with Linux's, which stays as it was.
- (macos-finish) 2026-09-29 — `port/macos-finish` was rebased onto `main` at
  4d24113 (Phase 3's path model and the Linux CI fixes) before it was handed
  back. The conflicts were the platform contract table's rows in
  `df-core/src/platform/mod.rs` (both sides' rows kept), df-core's
  `Cargo.toml` (Windows' `windows-sys` table and macOS's objc2 table, both
  kept) and ticks in `appendix-inventory-df-core.md` (both kept); the code
  merged without a conflict. Every `done <sha>` in this document names the
  rebased commit; the run ids name the runs that proved the commits before
  the rebase, and the branch's CI after it ran again on the rebased ones.
- (macos-finish) 2026-09-29 — M2.27 is written up as skipped rather than left
  `[~]` with no reason, so the Status line can say done: it was always
  optional (Decisions already made: software decode this phase), and only
  the §4.6 live check can call for it.
- (macos-finish) 2026-09-29 — The `trashview` twins trash real files into the
  runner's `~/.Trash` through a journal of their own, as M2.8's tests do,
  and destroy what they trashed before they end.
- 2026-09-29 — Brian-delegated, taken out of the Open questions: a Mac's
  opener rules are Linux's row for row with `open`, the system's default
  app, where delightviewer was, and a second `open` in a row named once, so
  Enter on a PDF or a song goes to the Mac's own app and `O` still offers
  Preview for a picture and mpv for a video (ebb95b8); `g D` is the Desktop
  and `g o` Documents on a Mac, and on Windows, which ships §6's column
  (f0d0573, 05's log); the sync refusal reads once, "Sync to a server needs
  rsync 3.1 or newer — `brew install rsync`", `RSYNC_HINT` being what the
  sentence names in place of the bare "rsync" and the M2.6 text corrected
  to match (c74fbc5); and the help sheet's "⌘-click toggles, ⌥-drag links"
  is left out and shown nowhere (05 D5.5).
- (macos-finish) 2026-09-29 — The hits' undo and redo of a trash run on every
  Unix; the redo's check asks the trash's `list()` and the item's
  `location()` in place of counting the freedesktop `files/` folder, which on
  Linux says the same thing (abdfa65). `app/tests/trash.rs` stays Linux's:
  its fixtures are `.trashinfo` records, the purge stamp and a `files/`
  folder to weigh, and on a Mac there is no one folder to weigh — the trash
  chip counts items and shows no size there (`weigh_trash` has nothing to
  walk).
- (mac-native) 2026-09-30 — M2.37 appended as a section of its own, Native
  feel, after Windows' §12: Brian's calls on the title bar and the menu bar,
  made that day. M2.36 was taken (Open With from Finder, from
  `port/release-2`), so this is the next free number.
- (mac-native) 2026-09-30 — M2.37: the ☰ menu's groups under the bar's
  names, each group whole and in the ☰ menu's order. **File**: New tab, New
  window; then Find's three rows (Search everywhere by name…, …inside
  files…, Filter this folder…), where Finder has Find. **Edit**: the Edit
  list; then Clipboard (Finder's Show Clipboard). **View**: the View list;
  then Sort, File type (in a file dialog) and Appearance, each still a list
  flown out of its row, as Finder's View has Sort By; then Tasks and Disk
  usage; then Command palette…. **Go**: the Go list, places and pin row
  included; then Places… and Trash. **Help**: Keyboard shortcuts. **Quit**
  is the application menu's Quit. A list of verbs that lands in another
  menu is laid in it after a separator, a list of one setting's choices
  stays a list, and a row the table does not know goes at the end of File,
  so every ☰ row has a home whatever is added to the ☰ menu later
  (`mirror::tests::every_row_of_the_app_menu_has_a_home_in_the_bar`).
  Labels are the ☰ menu's, in its sentence case, not a Mac's Title Case.
- (mac-native) 2026-09-30 — M2.37: Tasks and Disk usage are in View, not
  Window, because Window is AppKit's own items and nothing else: the
  mirrored menus' keys are shown and not answered (below), and Window's ⌘M
  has to answer.
- (mac-native) 2026-09-30 — M2.37: a mirrored row's key is shown and not
  answered. Its menu's delegate answers `menuHasKeyEquivalent:forEvent:
  target:action:` with NO (objc2-app-kit 0.2 leaves the method out for its
  out-pointers; it is declared on the delegate with the raw signature), so
  the key goes to the window, as before, and through the keymap. Answered,
  ⌘V in a rename prompt would paste files where it pastes text, a bare `y`
  typed into a prompt would copy the selection, and there would be two
  doors for one key. The cost is AppKit's flash of the menu's title when a
  key fires a row. The application menu's and Window's keys (⌘H, ⌥⌘H, ⌘Q,
  ⌘M) are AppKit's and answer, as winit's default menu's did.
- (mac-native) 2026-09-30 — M2.37: Window's Minimize has the standard ⌘M,
  which answers, so on a Mac ⌘M over a clip minimizes the window where
  `ctrl+m` (Mute, when media is hovered) would mute it; the ⌘M a Mac user
  expects wins over a binding only a pointer over a clip reaches.
- (mac-native) 2026-09-30 — M2.37: a key equivalent is one chord. A command
  row takes the chord of the binding whose label is the ☰ row's key; a key
  that is a sequence (`g h`) has none, a row that is not a command (a
  place, a file type) is not looked up, and a function key past F35 has
  none. A letter is lower case with ⇧ in the mask; shifted punctuation is
  its glyph (`<`, not ⇧`,`), as the label writes it.
- (mac-native) 2026-09-30 — M2.37: while a prompt, the help sheet or a card
  has the keyboard, every mirrored row is grey, as a Mac greys a window's
  menus under a sheet: the ☰ menu cannot be opened over any of them. A
  choice that arrives while one is up (a card that came up with the menu
  open) is dropped. An in-window menu that is up is put away before a
  chosen row runs, as a click on a row puts its menu away.
- (mac-native) 2026-09-30 — M2.37: in a file dialog the ☰ menu's last row
  is Cancel; the bar's is still "Quit delightfile", and `terminate:` ends a
  dialog session as M2.26's path does, which is Cancel.
- (mac-native) 2026-09-30 — M2.37: the rows are published every frame, and
  a menu is built from the latest only as it opens (never while it is
  open). The first rows are built at once and the bar handed to the
  application then, so winit's default menu is up only until the first
  frame. The cost, measured on the Linux build machine with the dev
  profile (which is optimized): about 51 µs for the rows, 25 µs to lay
  them out, 1 µs to compare them with the last, per frame, on macOS only;
  Linux and Windows build nothing. A throttle is the answer if a Mac's
  `DF_FRAME_LOG` shows it.
- (mac-native) 2026-09-30 — M2.37: a mirrored menu keeps what AppKit puts
  in it itself (Help's search, a View menu's Enter Full Screen): our rows
  carry tags at or above 0x44460000 and only those are taken out when a
  menu is built again, the new ones going where the old began. A chosen
  row's tag says which menu and which entry of that menu's list of
  actions, and a list is replaced only when its menu is built, so a tag
  always names the row that was on screen.
- (mac-native) 2026-09-30 — M2.37: window tabbing is off for the process
  (`NSWindow.allowsAutomaticWindowTabbing = NO`, before the bar is made):
  the window has tabs of its own, and with a process per window AppKit's
  Show Tab Bar and Merge All Windows would show one tab or do nothing.
- (mac-native) 2026-09-30 — M2.37: the band's `left_inset` is the rightmost
  light's right edge from the window's left edge, and the row starts a
  `GAP` past it, as Windows' row ends a `GAP` short of its buttons; its
  `height` is the lights' foot. 78 points stand in when AppKit has no
  buttons for the window or reports them at under a point (a window not
  yet laid out). No band in full screen, where AppKit's title bar comes
  down over the window only under the pointer, or for a window whose
  content does not run up under its title bar.
- (mac-native) 2026-09-30 — M2.37: the drag is a new seam function,
  `window::title_press`, asked in the `MouseInput` arm before egui, since
  AppKit begins a window drag only inside the press (`drag_window` hands
  winit's current event to `performWindowDragWithEvent:`). Windows' and
  Linux's bodies answer `false`; Windows' is in `windows/window.rs` and its
  band code (`titlebar.rs`) is untouched. A double click reads
  `AppleActionOnDoubleClick` from the user's defaults (which search the
  global domain): `Maximize` zooms, `Minimize` minimizes, `None` does
  nothing, macOS 15's `Fill`, which has no public call, zooms; unset, the
  older `AppleMiniaturizeOnDoubleClick` decides, and otherwise it zooms.
- (mac-native) 2026-09-30 — M2.37: with no ☰ button the first crumb's plate
  is inset from the row's corner by the chips' inset, as the button's was,
  and an elided path's `…`, which is text on no plate, by the row's
  padding; `F10`'s menu hangs from the corner the button would have been
  in (`App::menu_button`). Linux's and Windows' rows are the same to the
  point (`ui::tests::with_no_title_band_the_layout_is_what_it_was` pins
  the button's place on each of its five rows).
- (mac-native) 2026-09-30 — M2.37: Linux's and Windows' stand-in for the
  menu bar (`MENU_BUTTON = true`, `start`, `publish`, `take`) is one file,
  `platform/menubar.rs`, compiled `not(target_os = "macos")`, so neither
  `platform/linux/` nor `platform/windows/` changes for it.
- (mac-native) 2026-09-30 — M2.37: `fitted_size` asks AppKit how much
  taller than its content a window is with `FullSizeContentView` in the
  style, as the window is now made, rather than for a plain titled window.
- (mac-native) 2026-09-30 — M2.37: the runner test that builds the bar's
  menus and reads them back runs on libtest's thread, not the main one, so
  its `MainThreadMarker` is `new_unchecked`: the menus are made, read and
  dropped on that one thread and never reach the application or the
  screen.
- (mac-native) 2026-09-30 — M2.37: `04-windows.md` W4.39 said Linux and
  macOS answer no band; it now says Linux does and macOS did until M2.37
  (the ground rules' §7: the document is fixed in the change that made it
  untrue). Nothing else of Windows' plan or code changed.
- 2026-09-30 — Brian (delegated, through the coordinator) on M2.37's two
  open questions. **`setMovableByWindowBackground` stays off**: winit's
  view answers every press as one that can move the window, so with it on
  the whole window would drag — a rubber band in the list, a divider, a
  scrollbar, a drag of files out — and the band's empty space drags
  through `title_press` alone. **The traffic lights sit level with the
  row**, as Electron's `trafficLightPosition` and VS Code put them, rather
  than where AppKit keeps them, level with the row's upper half: after each
  layout they are centred on the row in the band, and put back where
  AppKit had them when the band goes (full screen). Both questions are
  taken out of Open questions below; whether AppKit drags the window by
  itself from the title bar's depth stays, for the live check.
- (mac-native) 2026-09-30 — M2.37, the lights: the platform is told the
  row by a new seam function, `window::band_row`, called after
  `title_regions` (Linux never calls it, Windows' does nothing, its buttons
  being its own), rather than a new argument to `title_regions`, whose
  Windows body is band code this change keeps out of. The row is its first
  line (`ui::Layout::band_row`): with a strip, the strip, whose middle is 23
  points down; with one tab, the top row's first 38 points, whose middle is
  27 down, so a prompt's second line, for an error, grows the row without
  moving the lights. AppKit's title bar is made twice as deep as that
  middle is low, 46 or 54 points where it was about 28, its top kept where
  it is, and each button centred in it (`caption::lights_at`) — Electron's
  arithmetic, read from its `window_buttons_proxy.mm`, which also says the
  buttons are not to be held (AppKit may replace them) and that AppKit
  lays its title bar out again on its own occasions. So the buttons are
  read after every layout and written only when the title bar's depth or a
  button's height from its foot is half a point or more from where they
  should be: the first time, when the row moves (a strip coming or going),
  and after AppKit has put them back. Where AppKit had them — the title
  bar's depth and each origin — is taken once, before the first move, and
  is where they go back to when the band goes. The band's `left_inset` is
  read as before; the lights only move down.

## Open questions

- Whether `NSApp.currentEvent` inside winit's `CursorMoved` dispatch is the
  `mouseDragged` event AppKit requires for `beginDraggingSession` (M2.12). If not,
  the fallback is an `NSView` subclass override of `mouseDragged:` — record which.
  (df-app, 2026-09-29: M2.12 is built on the first; only the live check in
  `07-verification.md` §4.5 can answer.)
- Ghostty on macOS: does `ghostty -e` work from `open -a`? Affects the `edit` opener
  default in `05-defaults-and-config.md`.
- (macos-finish) The trash's weight on a Mac (M2.9, M2.35): the chip beside
  the counter and the Empty trash card's question show a count and no size,
  since the du walk weighs one `files/` folder and a Mac's trash is items
  scattered through Finder's Trash that the journal names. Options: a task
  that weighs the journal's items (a walk of each `location()`) and gives
  `app/tests/trash.rs` macOS twins; or accept the count alone.
- (mac-native) Whether AppKit moves the window by itself from a press on
  one of the row's controls — a tab chip, a crumb — inside the see-through
  title bar's depth, which since the lights sit level with the row (M2.37)
  is 46 or 54 points, the row's whole depth, where AppKit's own is about
  28. winit's view answers every press as one that can move the window
  (`mouseDownCanMoveWindow`, which it does not override, is YES for a view
  that is not opaque), and AppKit may take that answer in a title bar's
  depth without being movable by its background; Electron and VS Code put
  clickable controls under a title bar deepened the same way, which says it
  does not. The live check (`07-verification.md` §4.1, M2.37) answers it.
  If a drag from a tab or a crumb moves the window, the fix is a
  `mouseDownCanMoveWindow` on winit's view that answers NO over the
  window's controls, a method added to winit's class at run time
  (`class_addMethod`) — a patch to a class that is winit's.
