# 05 — Defaults and config per platform

Status: **in progress**

Scope: every value that is "Brian's Arch machine" today and has to be a per-platform
table: directories, the opener and rule tables, bookmarks, the keymap conventions
and their labels, font directories, help and usage text, and which external
binaries each platform is expected to have. The tables are the decisions; the
tasks wire them in. Linux columns are the current values and do not change.

Runs alongside Phase 1 (S1.11 needs §1), Phase 2 (M2.17–M2.21 need §2, §3, §6) and
Phase 4 (W4.3, W4.22 need §2, §6).

Factual basis: `appendix-inventory-df-core.md` §2 `config`, `state`, `zoxide`,
`preview::cache`, `vfs::config`; `appendix-inventory-df-app.md` §2.4, §2.5, §2.8, §3.

## Decisions already made

- **macOS follows yazi's macOS convention, not Apple's.** Config stays at
  `~/.config/delightfile` (yazi uses `~/.config/yazi` on macOS), so a person moving
  from yazi finds it where they expect, and `vfs.toml` sharing with yazi keeps
  working. State and data follow the same XDG-with-fallback rule. Only the
  *thumbnail cache* and *zoxide database* use what the other program uses, because
  those are shared with other software.
- **Windows uses `%APPDATA%` for config and `%LOCALAPPDATA%` for state and data**,
  under `delightfile\`. `HOME` is not consulted on Windows; `USERPROFILE` is home.
- **Openers on Unix keep shell strings; Windows openers are argv lists** (Phase 4
  W4.3). The default tables below are written in each platform's form.
- **The shipped rule table is Brian's on Linux and generic elsewhere.** Linux keeps
  delightviewer, pinta, the `system-cmd-*` helpers and the `/mnt/schwabserverroot`
  bookmarks. macOS and Windows ship only what a fresh machine has, plus `zed` if
  installed (its absence is the ordinary "opener not found" toast).
- **Cmd is Ctrl on macOS** (M2.20) with the override table in §3. Windows keymap
  is identical to Linux.

## 1. Directories (`platform::dirs`, S1.11 / M2.18 / W4.24)

| Function | Linux (unchanged) | macOS | Windows |
|---|---|---|---|
| `home()` | `$HOME` | `$HOME` | `%USERPROFILE%` |
| `config_dir()` | `$XDG_CONFIG_HOME/delightfile` or `~/.config/delightfile` | same rule | `%APPDATA%\delightfile` |
| `state_dir()` (state file, trash journal, `portal-last-dir`) | `$XDG_STATE_HOME/delightfile` or `~/.local/state/delightfile` | same rule | `%LOCALAPPDATA%\delightfile\state` |
| `data_dir()` (freedesktop trash root on Linux only) | `$XDG_DATA_HOME` or `~/.local/share` | same rule (unused) | `%LOCALAPPDATA%\delightfile` |
| `cache_dir()` (thumbnails, shared with yazi) | `$TMPDIR`-or-`/tmp` + `yazi-<uid>` | `std::env::temp_dir()` + `yazi-<uid>` (yazi does the same on macOS: verify in `yazi-shared/src/xdg.rs` at implementation, record the finding) | `%TEMP%\yazi-<USERNAME>` (verify yazi's Windows suffix the same way) |
| `runtime_dir()` (portal sockets, Linux only) | `$XDG_RUNTIME_DIR` | `temp_dir()` | `temp_dir()` |
| `temp_dir()` (SFTP downloads `delightfile-vfs-<pid>`) | `std::env::temp_dir()` | same | same |
| yazi `vfs.toml` (read before ours) | `$XDG_CONFIG_HOME/yazi/vfs.toml` or `~/.config/yazi/vfs.toml` | same | `%APPDATA%\yazi\config\vfs.toml` (yazi's Windows config dir has the extra `config` level) |
| zoxide `db.zo` | `$_ZO_DATA_DIR` or `$XDG_DATA_HOME/zoxide` or `~/.local/share/zoxide` | `$_ZO_DATA_DIR` or `~/Library/Application Support/zoxide` | `$_ZO_DATA_DIR` or `%LOCALAPPDATA%\zoxide` |
| keymap, theme, config file names | `delightfile.toml`, `theme.toml`, `keymap.toml`, `vfs.toml` in `config_dir()` | same | same |

- [ ] **D5.1** Implement the table in `platform/{linux,macos,windows}/dirs.rs`
      (Linux body moved by S1.11). `config_dir()` and `state_dir()` create nothing;
      writers create on first write as today. Done when: a test per target asserts
      the paths with a controlled environment (`std::env::set_var` inside a lock,
      as the existing `state_path_from` tests do).
- [ ] **D5.2** `README.md` gets a "Where things live" table per platform. Done
      when: reviewed by Brian.

## 2. Openers and rules (`platform::defaults`, M2.17 / W4.3)

Linux: `DEFAULT_OPENERS` and `DEFAULT_RULES` at `crates/df-core/src/config.rs:192–375`,
unchanged.

### 2.1 macOS openers (shell strings, run by `$SHELL -c` with `$1`/`$@` positional)

| id | command | block | description |
|---|---|---|---|
| `open` | `open "$1"` | no | Open |
| `edit` | `"${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@"` if `$TERMINAL` or `ghostty` resolves on `PATH`, else `open -a Terminal "$@"`; encoded as `command -v "${TERMINAL:-ghostty}" >/dev/null 2>&1 && exec "${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@"; exec open -a Terminal "$@"` | no | Edit in $EDITOR |
| `zed` | `zed "$@" 2>/dev/null \|\| open -a Zed "$@"` | no | Open in Zed |
| `zed-workspace` | `zed "$1" 2>/dev/null \|\| open -a Zed "$1"` | no | Open folder in Zed |
| `terminal-here` | `open -a "${TERMINAL_APP:-Terminal}" "$1"` | no | Terminal here |
| `terminal-at` | `open -a "${TERMINAL_APP:-Terminal}" "$(dirname "$1")"` | no | Terminal at file |
| `open-in-chrome` | `open -a "Google Chrome" "$@"` | no | Open in Chrome |
| `play` | `command -v mpv >/dev/null 2>&1 && exec mpv --force-window "$@"; exec open "$@"` | no | Play |
| `edit-image` | `open -a Preview "$@"` | no | Edit image |
| `bulk-rename` | `zed --new --wait "$@"` | yes | Bulk rename in Zed |
| `extract`, `extract-here`, `extract-merged` | `builtin:*` | — | unchanged |

Dropped on macOS: `delightviewer`, `delightviewer-edit`, `set-wallpaper`,
`optimize-avif` (no such binaries). `TERMINAL_APP` is a delightfile-only variable
because `$TERMINAL` names a binary on Linux and `open -a` wants an app name.

### 2.2 Windows openers (argv strings, `$1`/`$@`/`$dir`, W4.3)

| id | command | block | description |
|---|---|---|---|
| `open` | `builtin:shell-open` | — | Open |
| `edit` | `code --wait "$@"`, else `notepad "$@"` (two candidates via `first_available`, D5.4; the splitter expands no environment variables, W4.3) | no | Edit |
| `zed` | `zed "$@"` | no | Open in Zed |
| `zed-workspace` | `zed "$1"` | no | Open folder in Zed |
| `terminal-here` | `wt -d "$1"` when `wt` is on `PATH`, else `cmd /K cd /d "$1"` (encoded as two candidates, first found wins: `platform::open::first_available(&[...])`) | no | Terminal here |
| `terminal-at` | `wt -d "$dir"`, else `cmd /K cd /d "$dir"` | no | Terminal at file |
| `open-in-chrome` | `chrome "$@"` — resolved on `PATH` only; when Chrome is installed but not on `PATH` the ordinary not-found toast appears and the person adds the full path in `delightfile.toml` | no | Open in Chrome |
| `play` | `builtin:shell-open` | — | Play |
| `bulk-rename` | `code --wait "$@"` else `notepad "$@"` | yes | Bulk rename |
| `extract*` | `builtin:*` | — | unchanged |

### 2.3 Rules

macOS and Windows use the Linux `DEFAULT_RULES` table minus the rows that name a
dropped opener (`delightviewer*`, `edit-image` → `open` on Windows, `set-wallpaper`,
`optimize-avif`). The `text/*` rule keeps `zed, edit, open, terminal-at`.

- [~] **D5.3** `platform::defaults::{OPENERS, RULES}` per target; `Config::default`
      (`config.rs`) reads them; the Linux tables are the existing constants
      re-exported from `platform/linux/defaults.rs` so the diff on Linux is a move.
      Done when: `config.rs` tests pass on Linux; a test per target asserts
      `openers_for("x.txt", "text/plain", false)` names `zed` first.
      — blocked on the macOS side: `platform::defaults` and `Config::default` are
      df-core's, and the df-app branch kept out of df-core (02-macos.md M2.17 has
      the options).
- [ ] **D5.4** `platform::open::first_available(candidates: &[&str]) -> &str` for
      the two-candidate Windows entries (and usable on macOS for `edit`). Done
      when: unit test with a fake `PATH`.
      — nothing to do on the macOS side (df-app, 2026-09-29): the macOS `edit` opener
      carries its fallback in its shell string (§2.1), so nothing there calls
      `first_available`; it is W4.3's, for the Windows argv entries.

## 3. Keymap

Windows: identical to Linux.

macOS (`platform::defaults::KEYMAP_OVERRIDES`, applied after the defaults and
before the user's `keymap.toml`, M2.17):

| Chord (as matched, Cmd = ctrl) | Command | Replaces |
|---|---|---|
| `ctrl+c` (Files) | `yank-to-system` (`Y`) | `close-tab` |
| `ctrl+w` (Files) | `close-tab` | — |
| `ctrl+v` (Files) | `paste-system` (`p` with nothing yanked) | — |
| `ctrl+c` (Input) | `input-copy` — a new command: copy the field's selection, or the whole text when nothing is selected, to the system clipboard | `overlay-close` |
| `ctrl+c` (Confirm, Pick, Tasks, Spot, Help, Palette) | unchanged `overlay-close` | — |
| `ctrl+q` | `quit` | — |
| `ctrl+,` | unbound (reserved for a future settings sheet) | — |

Labels (`df_core::platform::keys::LABELS`, M2.21): macOS `⌃` `⌥` `⇧` `⌘`, with the `ctrl`
role rendered `⌘` since Cmd is what people press; a binding the user wrote as
`super+x` in `keymap.toml` is parsed as `ctrl` on macOS (they are the same key
there) with a config warning "super is Cmd, which is Ctrl on macOS". Linux and
Windows: `Ctrl+` `Alt+` `Shift+` `Super+` as today.

- [~] **D5.5** Implement the override table and `input-copy`; the help sheet's
      mouse section says "⌘-click toggles, ⌥-drag links" on macOS. Done when: a
      `Keymap` test on macOS resolves `ctrl+c` in Files to `yank-to-system`.
      — blocked: the override table is df-core's keymap and `input-copy` a df-core
      command (02-macos.md M2.17 says why the df-app branch kept out of df-core).
      The help sheet's "⌘-click toggles, ⌥-drag links" needs a mouse section the
      sheet does not have yet; which section, and whether Linux gets its own line
      there, is part of this task.

## 4. External binaries

What each platform is expected to have, and what happens when it is absent (all
absences are already "missing feature, never a failure" on Linux; the table is for
the README and for choosing defaults).

| Binary | Used for | Linux | macOS | Windows |
|---|---|---|---|---|
| `7z` | 7z/rar/iso listing and extraction, `A` to 7z | p7zip | `brew install 7zip` (`7zz`: add `7zz` to `candidates("7z")` on macOS) | 7-Zip installer (`7z.exe` on PATH if the user adds it) |
| `bsdtar` / `tar` | fallback extractor | libarchive | `/usr/bin/tar` is bsdtar: add `tar` to `candidates("bsdtar")` on macOS | `tar.exe` (bsdtar) ships with Windows 10+ |
| `gzip`, `xz`, `zstd` | compressed tar list/extract/create | usually present | `gzip`/`xz`(via brew)/`zstd`(brew) | absent unless installed; `.tar.gz` listing falls back to `7z` or `tar.exe` (add `tar -tzf` as a listing path? No: keep the current "no decompressor" toast) |
| `git` | git chip | present | Xcode CLT | Git for Windows |
| `ssh` | SFTP, rsync | OpenSSH | built in | Windows OpenSSH (built in) |
| `rsync` ≥ 3.1 | `alt+p` sync | present | `brew install rsync` (stock 2.6.9 is refused with the hint, M2.6) | unsupported |
| `fd`, `rg` | search panel | present | brew | scoop/winget; absent → the existing "not installed" toast |
| `zoxide` | `Z` jumps | present | brew | scoop/winget |
| `gio`, `udisksctl`/udisks2, `wl-copy` | Linux integration | present | — | — |
| `setsid` | detach | util-linux | not needed (M2.16) | not needed |
| `xdg-open` / `open` / ShellExecute | default app | xdg-utils | built in | built in |
| `libpdfium` | PDF pages | manual | bundled in the `.app` | bundled in the zip |
| FFmpeg 9 shared libs | video, HEIF, audio | system | bundled | bundled |

- [x] **D5.6** `platform::fonts::dirs()` (S1.29, M2.19, W4.22):
      Linux unchanged; macOS `~/Library/Fonts`, `/Library/Fonts`,
      `/System/Library/Fonts`, `/System/Library/Fonts/Supplemental`; Windows
      `%LOCALAPPDATA%\Microsoft\Windows\Fonts`, `C:\Windows\Fonts`. The `PREFERRED`
      stems (`icons.rs:84–93`) are the same on every platform; on macOS the
      Homebrew cask installs `SymbolsNerdFontMono-Regular.ttf`, so add
      `SymbolsNerdFontMono-Regular` to `PREFERRED`. Done when: the `install` test
      with a fake directory passes on every target.
      — done 14120fe: the directories were S1.29's; `SymbolsNerdFontMono-Regular`
      ends `PREFERRED`, and
      `icons::tests::the_icon_face_is_found_by_preference_across_directories` runs
      on every target, the macOS runner included. On Linux the one difference is
      that face ranking ahead of a patched face the list does not name (Decisions
      log).
- [ ] **D5.7** `candidates()` additions from the table (`7zz`, `tar` on macOS;
      `tar.exe` on Windows is W4.2). Done when: unit tests.
- [ ] **D5.8** README "What it uses" table per platform from §4. Done when:
      reviewed.

## 5. Text

- [ ] **D5.9** `cli::USAGE` head says "a keyboard-first file manager" (no
      "for Wayland"); `platform::cli::EXTRA_USAGE` carries the `--portal` paragraph
      on Linux (S1.31). Crate descriptions in both `Cargo.toml`s and the `main.rs`
      doc comment updated in the same change. Done when: `--help` output snapshot
      test per target.
- [ ] **D5.10** Help sheet (`help.rs`) and which-key: no text change beyond the
      labels (M2.21). The `?` sheet's footer line naming the config path uses
      `platform::dirs::config_dir()` (verify it does not hard-code `~/.config`).
      Done when: grep finds no `.config/delightfile` literal in df-app.

## 6. Bookmarks

| Key | Linux (unchanged) | macOS | Windows |
|---|---|---|---|
| `g ~` | `~` | `~` | `~` |
| `g c` | `~/.config` | `~/.config` | `%APPDATA%` |
| `g d` | `~/Downloads` | `~/Downloads` | `~/Downloads` |
| `g w` | `~/Work` | `~/Work` | `~/Work` |
| (others) | `/mnt/schwabserverroot…`, `sftp://showandtour1/2` | `~/Desktop`, `~/Documents` | `~/Desktop`, `~/Documents` |

- [~] **D5.11** `platform::defaults::BOOKMARKS`; Linux is the existing constant
      moved. Done when: `keymap/defaults.rs` reads it and tests pass.
      — blocked: `keymap/defaults.rs` is df-core's (02-macos.md M2.17).

## Decisions log

- 2026-09-25 — `~/.config/delightfile` on macOS (yazi parity) rather than
  `~/Library/Application Support`.
- 2026-09-25 — Generic opener tables off-Linux; Brian's personal helpers stay Linux
  defaults.
- 2026-09-25 — `TERMINAL_APP` env var for `open -a` on macOS.
- 2026-09-25 — Cmd+C/V/W overrides on macOS; `input-copy` command added for prompts.
- (df-app) 2026-09-29 — D5.6: `SymbolsNerdFontMono-Regular` is named last in
  `PREFERRED` on every target, as planned. It was already found on any target as
  a patched face the list does not name; naming it only ranks it above those,
  which on Linux matters only on a machine with that face and another unnamed
  one and none of the named four.
- (df-app) 2026-09-29 — D5.4 has no macOS side: see the task.

## Open questions

- yazi's exact cache-dir suffix on macOS and Windows (verify at D5.1).
- Whether `wt` should be the Windows terminal default (see `04-windows.md`).
- Ghostty `-e` on macOS (see `02-macos.md`).
