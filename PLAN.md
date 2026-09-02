# delightfile — plan

A native, GPU-rendered file manager for Linux/Wayland that replaces yazi. Keyboard-first
with Brian's yazi muscle memory, but with everything a terminal cannot do: real
drag-and-drop, rich media previews with video shuttle, undo, animated motion design, and
mouse support that feels like delightstack.

**One deliberate break from yazi**: vim-style `hjkl` navigation is removed. Arrow keys
navigate; `j` `k` `l` are delightviewer's video transport keys *globally* — arrow onto a
video and immediately play/shuttle it without leaving the list (§4.3).

Sibling projects this plan borrows from directly:

- **delightviewer** (`~/Work/delightviewer`) — the architectural template: winit + egui +
  egui-wgpu, headless core crate, hand-rolled TOML/D-Bus, pure-function motion, hover
  system, keymap engine, toasts, which-key card, shuttle transport.
- **DelightMail** (`~/Work/mail.brianschwabauer.com`) — the keyboard registry that feeds
  which-key, `?` help, and the command palette from one table.
- **Brian's yazi config** (`~/.config/yazi/`) — the default keymap, opener rules, layout,
  theme, and scripts, ported binding-for-binding.

Skills to load while building UI: `delightful-ui`, `ui-anti-slop`.

---

## 1. Stack & workspace

Cargo workspace, `resolver = "2"`, mirroring delightviewer's layout and lints:

```
delightfile/
  crates/
    df-core/     # headless: fs model, sort/filter, op journal + undo, task engine,
                 # keymap engine, config parsing, trash, zoxide db, archive walking.
                 # NO winit/egui/wgpu — unit-testable without a window.
    df-app/      # the binary. winit ApplicationHandler + egui + egui-wgpu shell,
                 # all painting, hover/gesture/motion, drag-and-drop, clipboard, D-Bus.
    dv-core/     # vendored from delightviewer (as it vendors from delightvideo)
    dv-media/    # decode: images, video frames, thumbnails
    dv-playback/ # video/audio playback for the preview pane
```

- Dependencies: `egui`/`egui-wgpu`/`egui-winit` (matching delightviewer's pinned versions),
  `winit`, `pollster`, `thiserror`, `log`, `env_logger`, `crossbeam-channel`,
  `parking_lot`, `serde`. **wgpu only via `egui_wgpu::wgpu`.** No `toml` crate (hand-rolled
  parser, ported from delightviewer's `parse_toml_tables`), no zbus (hand-rolled D-Bus,
  ported from delightviewer's `dbus.rs`). New crates only when rewriting is impractical;
  prefer what's already in the tree.
- Workspace lints: `unsafe_code = "warn"`, `clippy::all = "warn"`,
  `clippy::unwrap_used = "warn"`. Dev profile `opt-level = 1` (deps at 2), release
  `lto = "thin"`.
- Errors: `thiserror` enums per crate. Naming: idiomatic Rust `snake_case`,
  `CAPS_CASE` consts with a comment defending every constant's value.
- Event loop: repaint-on-event, never a poll loop. Worker threads wake the loop with a
  zero-sized `Wake` user event. Idle cost is a design constraint: every animation must
  stop scheduling frames the moment it has arrived (delightviewer's `animating()`
  distinction between "still fading" and "held hot").
- Pure-function physics: motion, shuttle, hover, and gesture state machines are functions
  over `(inputs, Instant)` with `#[cfg(test)]` tests. No eyeballed motion.

### Non-goals

- No Lua/plugin scripting. The three plugin needs (piper/eza dir preview, AVIF stills,
  office docs) become built-ins or openers.
- No light theme initially (yazi config pins catppuccin-mocha for both).
- Wayland-first. X11 only if it falls out of winit for free.
- Not a terminal: `;`/`:` shell commands spawn in `$TERMINAL` or run headless with
  output in a toast/panel — no embedded terminal emulator in v1.

---

## 2. Layout & panes

Yazi's miller columns at Brian's ratio `[1, 4, 3]`: **parent | list | preview**.

- `sort_dir_first`, alphabetical, case-insensitive, `linemode = size`, hidden files off,
  `scrolloff = 5` — all as current yazi defaults, all configurable.
- **Tabs**: `t` new tab, `1`–`9` switch, `Alt+[`/`Alt+]` prev/next, `{`/`}` swap
  (`[`/`]` now belong to transport, §4.3). Tab strip only visible with 2+ tabs.
- **Grid view**: per-directory toggle between list and thumbnail grid (for Pictures,
  plex mounts). Remembered per directory in a small state db
  (`~/.local/state/delightfile/`). Sort changes animate rows/tiles to their new
  positions (FLIP-style) rather than teleporting.
- **Multi-window**: `Ctrl+N` new window; drag a tab out to spawn a window; drag files
  between windows/tabs (drop on a tab header targets that tab's cwd).
  **A window is a process.** `Ctrl+N` and a detached tab both start a new `delightfile`
  on that directory rather than adding a second window to this event loop — the reasoning,
  the cost, and what an in-process split would take are written out in `df-app/src/window.rs`.
  Window→window file drags therefore go through the compositor's data device, which is the
  cross-application path Phase 4b already built; the state file is last-writer-wins across
  windows; and `--cwd-file` belongs to the process that was launched with it and is never
  inherited by a window spawned from it.
- Breadcrumb path bar at top: clickable segments, shows git branch when inside a repo,
  drop target per segment. **It is the only bar.** The row also carries, on its right,
  the position counter and the status chips (selection, visual, clipboard); a committed
  filter is a trailing crumb-like chip; and a non-anchored prompt takes the crumbs' place
  in the same row rather than opening a line of its own. Modal surfaces carry their own
  hints along the bottom edge of their card.

### 2.1 Keyboard focus: there isn't any

**The middle column is the keyboard, always.** There is no pane focus, no focused-pane
treatment, and no key that moves the keyboard from one column to another. A file manager
is a list of files with two columns of context beside it; a three-way focus meant every
key had to be read twice — once for what it does, once for where you were when you
pressed it — and the second reading was never worth its cost.

- **Rightward**: `→` on a directory enters it (yazi behavior); `→` on an *archive*
  browses it (§7.3); `→` on a plain file does **nothing** — the preview beside it is
  already showing that file, so there was never anywhere for the key to go. It is
  deliberately not "open": `→` is a navigation key, and a navigation key that could
  launch a video player is a key you stop pressing. **Leftward**: `←` goes to the parent
  (yazi `leave`).
- `Esc` is a priority ladder, and its last rung is clearing the selection — there is no
  focus underneath to return from.
- The parent and preview columns are **fully live under the pointer**: a click on a
  parent row goes there, the wheel scrolls whichever column it is aimed at, and the
  preview's transport and scrubber take clicks and drags. None of that changes what the
  keyboard means, because there is nothing for it to change.
- The preview's own keys are reached from the list, on the file under the cursor — see
  §4.3. `Ctrl+arrow` is the preview's arrow family; the plain arrows are always the
  list's.
- Visuals: every pane is painted the same. The list's cursor row is at full strength and
  the parent column's marker sits at 35% — not because the keyboard is elsewhere, but
  because "where you are" is a quieter fact than "what you are about to act on".

With `hjkl` gone, the transport keys are global: arrow onto a video and press `k` and it
plays, wherever the pointer happens to be.

---

## 3. Config

`~/.config/delightfile/` via `$XDG_CONFIG_HOME`, hand-rolled TOML parsing. Missing files
are silent; invalid lines warn human-readably while valid ones still apply.

- `delightfile.toml` — layout ratio, sorting, linemode, show_hidden, scrolloff,
  previews, task workers, opener rules, goto bookmarks.
- `keymap.toml` — user overrides on top of built-in defaults (delightviewer's model:
  defaults in code, file amends). Transport keys `j k l [ ]` are hard-reserved
  globally (delightviewer's `RESERVED_TRANSPORT_KEYS` rule), rejected on override.
- `theme.toml` — palette overrides, per-directory icon/color rules (port the 20 custom
  dir icons from yazi's theme.toml as defaults).
- `vfs.toml` — read yazi's format directly for SFTP services (§7.6).
- **Defaults ARE Brian's current yazi config.** Ship with the keymap in §4, the opener
  rules in §6, ratio `[1,4,3]`, catppuccin-mocha. A fresh install is day-one home.

CLI: `delightfile [path]`, `--cwd-file=<path>` (write cwd on quit — keeps the Hyprland
`Super+F` binding working by swapping `yazi` → `delightfile`), `--choose-file`/
`--choose-dir` for picker mode (future: XDG file-chooser portal backend).

---

## 4. Keyboard system

One registry of `(context, chord, when-predicate) → Command` feeds four surfaces:
dispatch, the which-key card, `?` help, and the command palette. Contexts stack
most-specific-wins: `Global / Files / Input / Confirm / Pick / Tasks / Spot / Help /
Palette`. Chord prefixes (`g`, `m`, `c`, `,`, `Space` sequences) show the **which-key
card after 175 ms** of an unfinished chord — fast typists never see it. `[which]`
ordering preserves declaration order, not alphabetical.

### 4.1 Files context (yazi `[mgr]` parity, minus `hjkl` — the muscle-memory contract)

| Keys | Action |
|---|---|
| `Esc` | ladder: close overlay → cancel chord → close prompt → leave visual → clear filter → clear selection |
| `q` / `Q` | quit (writing cwd-file) / quit without |
| `Ctrl+c` | close tab (quit if last) |
| `↑` / `↓` | cursor up/down |
| `Ctrl+u/d` `Ctrl+b/f` | half page / full page |
| `g g` / `G` | top / bottom |
| `←` / `→` | leave / enter directory (a file has no inside — §2.1) |
| `Alt+←` / `Alt+→` | history back / forward (browser-style; `H`/`L` are retired) |
| `j` `k` `l` `L` `[` `]` `<` `>` | media transport on the hovered file — global (§4.3) |
| `Space` | toggle select + advance |
| `Ctrl+a` / `Ctrl+r` | select all / invert |
| `v` / `V` | visual select / visual unset |
| `K` / `J` | scroll/seek preview ±5 (yazi parity) |
| `Tab` | spot panel on hovered file |
| `o`/`Enter` | open (first opener rule) |
| `O`/`Shift+Enter` | open interactively (picker) |
| `y` / `x` / `p` / `P` | yank / cut / paste / paste --force |
| `Y` | mime-aware copy to system clipboard (§7.4) |
| `X` | unyank |
| `-` / `_` / `Ctrl+-` | symlink abs / rel / hardlink |
| `d` / `D` | trash / delete permanently (both undoable where possible, §5) |
| `a` | create (trailing `/` = dir) |
| `r` / `R` | rename (cursor before ext) / rename empty stem |
| `;` / `:` | shell / shell --block |
| `.` | toggle hidden |
| `s` / `S` | search by name (fd) / by content (rg) — native overlay (§7.2) |
| `z` / `Z` | fuzzy jump overlay / zoxide frecency jump |
| `M` | mount manager (udisks2 over D-Bus) |
| `m s/p/b/m/o/n` | linemode: size/perms/btime/mtime/owner/none |
| `c c/d/f/n` | copy path/dirname/filename/stem |
| `c t` | copy file's text contents (binary → falls back to yank) |
| `f` `/` `n` `N` | filter --smart, find, next/previous match (`?` is help) |
| `, m/M b/B e/E a/A n/N s/S r` | sort (time & size sorts also switch linemode, as now) |
| `g h/c/d/w/s/p/a/r/1/2/Space/f` | goto: ~, ~/.config, ~/Downloads, ~/Work, server, plex, archive, git root, sftp1, sftp2, interactive, follow symlink |
| `t` `1–9` `Alt+[`/`Alt+]` `{`/`}` | tabs (as §2; `[`/`]` belong to transport) |
| `w` | task manager |
| `u` / `Ctrl+Shift+z` | **undo last operation (new)** |
| `Ctrl+p` | **command palette (new)** |
| `~`/`F1` / `?` | help / keymap browser |

`[tasks]`, `[pick]`, `[confirm]`, `[cmp]`, `[help]`, `[spot]` contexts: port yazi's
defaults verbatim (spot: `h`/`l` swipe prev/next file, `c c` copy cell, etc.).

### 4.2 Input context — one plain line editor

**Nothing modal.** No Normal mode, no `INSERT` chip, no block caret, no
`[input] vi_mode`: every unmodified key types itself, and `Esc` cancels the
prompt on the first press, everywhere. A config that still sets `vi_mode` gets a
warning saying so.

Readline's map is what is left, and it is all the editing anybody reached the
modes for: `Ctrl+a`/`Ctrl+e`/`Home`/`End`, `Ctrl+b`/`Ctrl+f` and the arrows,
`Alt+b`/`Alt+f` and `Ctrl+arrow` by the word, `Ctrl+u`/`Ctrl+k`/`Ctrl+w`/`Alt+d`
to kill, `Ctrl+h`/`Ctrl+d`/`Backspace`/`Delete`, `Ctrl+z`/`Ctrl+y` to undo and
redo, and `Shift` on any motion to extend a selection that typing or deleting
then replaces. One implementation used by rename, filter, create, cd, search,
shell. Custom prompt titles and popup geometry as in yazi.toml (rename anchored
to the hovered row; others top-center). No cursor blink.

### 4.3 Transport — delightviewer semantics, global

**Global core** (acts on the media file the cursor is on — arrow to a video,
press `l`, it's shuttling):

- `k` play/pause — "the play key that is only ever the play key".
- `j` / `l` shuttle reverse/forward. Ladder = powers of two, 1×→128× (`SHUTTLE_MAX`).
  Discrete presses double instantly; held key-repeat is paced to one doubling per
  550 ms (`SHUTTLE_HOLD_STEP`); direction change restarts at 1×. Pause or frame-step
  drops off the ladder so the next press plays at 1×. Audio audible ≤2× forward,
  muted above. Rate badge lingers then fades.
- `L` toggle loop. `[` / `]` prev/next edge (chapters if present; else start /
  one-frame-before-end). `<` / `>` ±10 s. `Shift+↑`/`Shift+↓` volume (step 0.1,
  max 2.0).
- On a non-media file these keys are inert (no beep, no surprise). `j`/`k`/`l`/`[`/`]`
  are hard-reserved; user overrides rejected.

**The preview's own keys, from the list.** The keyboard never enters the preview pane
(§2.1), so these act on whatever the cursor is standing on, and none of them may take a
key the list already owns. `Ctrl+arrow` is the preview's arrow family — one modifier,
four directions — and the sideways pair splits on what is under the cursor exactly as the
transport does:

| Key | On a clip | On a document / image |
|---|---|---|
| `Ctrl+←` / `Ctrl+→` | frame step back / forward | previous / next page |
| `Ctrl+↑` / `Ctrl+↓` | — | scroll a line, or step a G-code layer |
| `Ctrl+Shift+u` / `Ctrl+Shift+d` | — | half page up / down |
| `Shift+Space` | — | page down |
| `Ctrl+Home` / `Ctrl+End` | — | top / bottom |
| `+` `=` / `Alt+-` / `0` | — | zoom in / out / reset |
| `Ctrl+m` | mute | — |
| `k`, `Shift+↑`/`Shift+↓` | play/pause, volume (global, above) | — |

Zoom out is the one key in the family that had to move: `-` is the absolute symlink in
§4.1 and a file operation outranks a zoom, so it takes the `Alt` spelling of the same key.
Paging past the last page (or before the first) is inert — there is no pane to fall out
of. Hover the timeline to scrub; the position strip lingers 2.5 s after interaction.

### 4.4 Command palette

`Ctrl+p`. Fuzzy list of every registered command with its chord rendered inline, plus
goto bookmarks and recent directories. Same registry as which-key/help — the keymap
documents itself in three places from one table. DelightMail's palette is the reference.

---

## 5. File operations, undo, tasks

- **Op journal in df-core.** Every mutation (move, rename, copy, trash, symlink,
  create) records its inverse. `u` undoes; each op lands with an **undo toast (8 s)**.
  Trash uses the freedesktop trash spec (hand-rolled) so `d` is always reversible;
  `D` permanent delete gets a confirm dialog (70×20-equivalent, custom body text) and
  is the one thing undo can't restore.
- **Task engine**: worker pool (10/10 as configured), per-file progress, pause/resume,
  cancel, queue reordering in the `w` panel. Copies are reflink-first on btrfs/XFS
  (`FICLONE`, fall back to read/write), with optional verify. `bizarre_retry = 3`.
- **Conflict resolution**: on name collision, a dialog showing both files' thumbnails +
  metadata side by side — overwrite / skip / rename / apply-to-all. Not a text prompt.
- **Bulk rename**: selection → `r` opens an in-app two-column editable diff (old → new)
  with live validation, instead of shelling to an editor. The `bulk-rename.txt` opener
  path stays for compatibility.
- **Toasts** (delightviewer's rules): one at a time, never stacked. Lifetimes by kind —
  notice 2.2 s, undo 8 s, confirm 5 s, sticky for ongoing states (a paused task shows a
  sticky toast; a message that expires while its subject is still on screen lied).

---

## 6. Previews & openers

Preview pipeline in df-core + dv-media, decode workers started **before the window**
on cold start (delightviewer's ordering); results crossfade in over ~80 ms.

- **Thumbnail cache**: keep the yazi-compatible cache handshake delightviewer already
  reads, so delightfile ⇄ delightviewer hand off the exact frame the user was looking
  at. One shared cache dir.
- **Built-in previewers**: text with syntax highlighting (tmTheme from the vendored
  catppuccin flavor), rendered markdown (replaces glow), images incl. AVIF/HEIF via
  dv-media (replaces the hand-written avif plugin), video with hover-scrub + shuttle,
  audio with waveform, PDF via `pdfium-render` dlopen'd at runtime (missing lib =
  missing feature, never a failure), font specimen sheets (ttf-parser), 3D models
  (stl/obj/ply/3mf) turntable, G-code layer view (reuse delightviewer's), directory
  listing (one-level tree with icons — replaces piper/eza), archive contents.
  Office docs: defer; keep the opener path.
- **Spot panel** (`Tab`): full metadata — EXIF, media duration/dimensions/codec,
  permissions with a clickable editor, owner, timestamps, checksum on demand, symlink
  target. `h`/`l` swipes between files without closing.
- **Openers**: port the yazi opener rules as config defaults — delightviewer for all
  media/PDF/fonts/3D/gcode, `$EDITOR`/zed for text, zed-workspace + terminal-here for
  dirs, the wallpaper/optimize/pinta entries — launched detached (`setsid uwsm-app --`
  style, configurable launcher prefix). `O` shows the picker anchored to the hovered
  row. Fix the yazi gap: archives get an extract opener rule by default.

---

## 7. Power features

### 7.1 Drag and drop (the native killer feature)

- **Drag out** to any app (Chrome uploads, Zed, image editors): `text/uri-list` +
  mime offer via the Wayland data-device protocol (hand-rolled client code in df-app,
  in the spirit of the hand-rolled D-Bus; smithay-client-toolkit only if hand-rolling
  proves impractical — this is the plan's biggest technical risk, prototype early).
- **Drop in** from other apps (winit `DroppedFile` + data-device for richer offers):
  drop onto any pane, tab header, or breadcrumb segment. Dropping browser images/text
  saves them as files.
- **Internal DnD**: drag rows to folders, across panes/tabs/windows. Modifier switches
  copy/move/link with a cursor badge.
- **Choreography** (delightful-ui): multi-file drags show a stacked-card ghost with a
  count badge; valid targets highlight as the drag approaches; commit thresholds fill
  a badge delightviewer-dismiss-style; cancel springs back (BackOut).
- **Selection basket**: collect files from multiple directories (`b` to toss in),
  shown as a floating tray; paste or drag the whole basket as one payload.

### 7.2 Search & jump

- `s`/`S` shell out to `fd`/`rg` (they're installed and excellent) but stream results
  into a native overlay with live preview of the hovered hit, not an fzf takeover.
- `z` fuzzy-jump overlay over history + bookmarks; `Z` reads the zoxide db directly
  (simple format, no subprocess) and ranks by frecency.
- `f` filter-as-you-type with smart-case and match highlighting in-row.

### 7.3 Awareness

- **Git**: status dots per row, gitignored files dimmed, branch in breadcrumb, dirty
  count. Read `.git` directly or shell `git status --porcelain -z` per directory
  (cheap, cached, async).
- **"What's big" mode**: background recursive du with animated per-row size bars and
  a treemap-ish drill-down — the ncdu replacement. Entered via palette or `, s` twice.
- **Archives as directories**: `l` walks into zip/tar/7z (read-only v1), previews
  inside, extract selection with progress.

### 7.4 System integration

- **Clipboard**: `Y` = native port of clipboard.sh — images offered with real image
  mime, text-likes as plain text, big/binary/multi as `text/uri-list`; >50 MB always
  URI. `c t` = copy text contents, binary falls back to yank. Paste (`p`) accepts
  files *and* clipboard images (saves a PNG). All via Wayland data-control/data-device,
  toast feedback instead of notify-send.
- **Mounts** (`M`): udisks2 over hand-rolled D-Bus — list/mount/unmount/eject, with
  removable drives also shown in the goto/palette surfaces.
- **Trash**: freedesktop spec, with a virtual trash:// location to browse/restore.

### 7.5 Mouse (beyond DnD)

Single click = hover/cursor; double = open; middle = new tab; right = context menu
mirroring opener rules + operations (with shortcuts rendered inline); scroll with
momentum; band-select with a drag rectangle; Ctrl/Shift-click selection. Parent pane
rows clickable. Every control: instant hover, ripple on click, press effect.

### 7.6 Remote (later phase)

SFTP from `vfs.toml` (`g 1`/`g 2` → showandtour1/2): ssh2-backed vfs in df-core with
async listing, download-on-open, upload-on-drop, cached thumbnails.

---

## 8. Design & motion

Load `delightful-ui` + `ui-anti-slop` before painting anything. The motion vocabulary
is delightviewer's painter-side port of delightstack — reuse its code and constants:

- **Instant in, animated out** (`hover.rs` port): hover/press amounts snap to 1,
  ease out over `FADE = 0.24 s` / `PRESS_FADE = 0.16 s`; `dt` clamped to 0.25 s;
  epsilon-pruned; `animating()` keeps idle windows at zero repaints.
- **Ripples** on click for rows, buttons, tabs — radial expand from the pointer, eased
  alpha, painter-drawn.
- **Easing**: CSS-semantics `cubic_bezier`. `OutQuint (0.22, 1, 0.36, 1)` for
  momentum/slides, `BackOut (0.34, 1.30, 0.55, 1)` for spring-backs. Durations:
  ~120 ms for state fades, 200 ms zoom, 250–600 ms slides (viewport-scaled), 400 ms
  spring-back. Momentum is one eased animation, never a physics loop.
- **Scrims & gradients**: any fade-to-transparent is eased (raised cosine,
  `SCRIM_BIAS = 2.5`, single contiguous mesh — never a two-stop linear ramp, never
  rect+mesh seams).
- **FLIP re-sorts**: rows animate to new positions on sort/filter changes.
- **Rounding**: squircle-feel rounded cards; nested rounding is concentric —
  container radius = element radius + gap.
- **Theme**: catppuccin-mocha palette as oklch-derived tokens; the 20 custom directory
  icon colors from theme.toml as defaults; per-filetype icon colors.
- **Linger then leave**: transient chrome (rate badge, position strip, hints) holds
  ~2.5–3 s then fades in ~500 ms, with one scheduled wake-up in between.
- Every constant gets a comment defending its value.

---

## 9. Testing

- df-core: unit tests for sort/filter, keymap dispatch (context stack, chords,
  predicates, reserved keys), op journal invertibility, TOML parser, trash spec,
  zoxide db reader, task state machine.
- df-app pure functions: hover, ripple, shuttle ladder (`shuttle_step` port keeps its
  tests), FLIP interpolation, scrim mesh — all `Instant`-parameterized.
- A `tests/fixtures/` tree of gnarly filenames (unicode, newlines, 255-byte names)
  exercised by op tests.

---

## 10. Roadmap

Work top to bottom; tick boxes in the same commit as the work.

### Phase 0 — skeleton
- [x] Workspace scaffold: df-core, df-app, vendor dv-core/dv-media/dv-playback; lints, profiles
- [x] winit ApplicationHandler + egui + egui-wgpu window, repaint-on-event, Wake event
- [x] Hand-rolled TOML config loading (`delightfile.toml`, `keymap.toml`, `theme.toml`)
- [x] Keymap engine: registry, context stack, chords, `when` predicates, reserved keys
- [x] Hover/press system + easing module ported from delightviewer, with tests

### Phase 1 — browse
- [x] Directory model: async read, watch (inotify), sort modes, dir-first, hidden toggle
- [x] Three-pane miller render `[1,4,3]`, cursor, scrolloff, linemodes, icons + theme colors
- [x] Navigation: arrow keys, gg/G, paging, Alt+←/→ history, `g` goto table, `--cwd-file`
- [x] Selection: Space, visual mode, Ctrl+a/r; filter `f`, find `/ n N`
- [x] Tabs: t, 1–9, [/], {/}, tab strip
- [x] Which-key card (175 ms), `?` help browser

### Phase 2 — operate
- [x] Task engine: workers, progress, pause/cancel, `w` panel
- [x] yank/cut/paste (+force), reflink-first copy, symlink/hardlink
- [x] Trash (freedesktop) + permanent delete + confirm dialogs
- [x] Create `a`, rename `r`/`R` with vi input editor
- [x] Op journal + `u` undo + toast system (one-at-a-time, kind lifetimes)
- [x] Conflict dialog with side-by-side previews
- [x] Shell `;`/`:`, open `o`/`O` with opener rules + picker

### Phase 3 — preview
- [x] Preview workers + yazi-compatible thumbnail cache; 80 ms crossfade-in
- [x] Text/syntax, rendered markdown, directory tree, image (incl. AVIF/HEIF)
- [x] Video/audio playback in pane (dv-playback); `K`/`J` seek ±5 from list
- [x] No pane focus: the list is always the keyboard; `→` enters a directory or an archive
- [x] Global transport: j/k/l shuttle ladder, L [ ] < >, volume; `Ctrl+←`/`Ctrl+→` frame step
- [x] Document scroll/zoom keys from the list (`Ctrl+arrow` family); PDF (pdfium dlopen), fonts, 3D, gcode
- [x] Spot panel `Tab` with metadata, permissions editor, checksum

### Phase 4 — mouse & drag
- [x] Click/double/middle/right + context menu, band select, momentum scroll
- [x] Ripples, press effects on all controls
- [x] Internal DnD with ghost stack, target highlight, spring-back cancel
- [x] Wayland data-device: drag out (uri-list), drop in from external apps
- [x] Clipboard native: `Y`, `c t`, paste files/images

### Phase 5 — power
- [x] Command palette `Ctrl+p` from the registry
- [x] `s`/`S` fd/rg streaming overlays with live preview; `z`/`Z` zoxide db + fuzzy jump
- [x] Grid/thumbnail view with per-dir memory; FLIP animated re-sorts
- [x] Git status dots, ignored dimming, branch breadcrumb
- [x] Archives as read-only directories + extract with progress
- [x] "What's big" du mode
- [x] Bulk rename diff view; selection basket
- [x] Mount manager `M` (udisks2 D-Bus)

### Phase 6 — reach
- [x] SFTP vfs from vfs.toml (`g 1`/`g 2`)
- [x] Multi-window; drag tabs out; cross-window DnD
- [x] Trash browsing/restore view
- [x] Idle-cost audit (zero repaints at rest), cold-start ordering audit
- [x] Polish pass against `delightful-ui`/`ui-anti-slop` checklists
