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
  bookmarks. macOS ships only what a fresh machine has, plus `zed` if
  installed (its absence is the ordinary "opener not found" toast). Windows'
  is a Windows user's (Brian, 2026-10-01, `04-windows.md` §13): Enter does
  what a double-click in Explorer does, `O` offers what this machine has and
  ends in "Open with…", and the bookmarks are the profile, the known folders,
  the system drive and `%APPDATA%`.
- **Cmd is Ctrl on macOS** (M2.20) with the override table in §3. Windows keymap
  is identical to Linux.

## 1. Directories (`platform::dirs`, S1.11 / M2.18 / W4.24)

| Function | Linux (unchanged) | macOS | Windows |
|---|---|---|---|
| `home()` | `$HOME` | `$HOME` | `%USERPROFILE%` |
| `config_dir()` | `$XDG_CONFIG_HOME/delightfile` or `~/.config/delightfile` | same rule | `%APPDATA%\delightfile` |
| `state_dir()` (state file, trash journal, `portal-last-dir`) | `$XDG_STATE_HOME/delightfile` or `~/.local/state/delightfile` | same rule | `%LOCALAPPDATA%\delightfile\state` |
| `data_dir()` (freedesktop trash root on Linux only) | `$XDG_DATA_HOME` or `~/.local/share` | same rule (unused) | `%LOCALAPPDATA%\delightfile` |
| `cache_dir()` (thumbnails, shared with yazi) | `$TMPDIR`-or-`/tmp` + `yazi-<uid>` | `std::env::temp_dir()` + `yazi-<uid>` (yazi does the same on macOS: verify in `yazi-shared/src/xdg.rs` at implementation, record the finding) | `%TEMP%\yazi-0` (yazi's `uid_or_zero`; Decisions log) |
| `runtime_dir()` (portal sockets, Linux only) | `$XDG_RUNTIME_DIR` | `temp_dir()` | `temp_dir()` |
| `temp_dir()` (SFTP downloads `delightfile-vfs-<pid>`) | `std::env::temp_dir()` | same | same |
| yazi `vfs.toml` (read before ours) | `$XDG_CONFIG_HOME/yazi/vfs.toml` or `~/.config/yazi/vfs.toml` | same | `%APPDATA%\yazi\config\vfs.toml` (yazi's Windows config dir has the extra `config` level) |
| zoxide `db.zo` | `$_ZO_DATA_DIR` or `$XDG_DATA_HOME/zoxide` or `~/.local/share/zoxide` | `$_ZO_DATA_DIR` or `~/Library/Application Support/zoxide` | `$_ZO_DATA_DIR` or `%LOCALAPPDATA%\zoxide` |
| keymap, theme, config file names | `delightfile.toml`, `theme.toml`, `keymap.toml`, `vfs.toml` in `config_dir()` | same | same |

- [x] **D5.1** Implement the table in `platform/{linux,macos,windows}/dirs.rs`
      (Linux body moved by S1.11). `config_dir()` and `state_dir()` create nothing;
      writers create on first write as today. Done when: a test per target asserts
      the paths with a controlled environment (`std::env::set_var` inside a lock,
      as the existing `state_path_from` tests do).
      — done f2196c8, green on the Windows runner at run 36666369775: the Windows
      column, tested with a controlled environment (`platform::windows::dirs`
      reads a variable table the tests hand it) and against the runner's own;
      Linux keeps the Unix rule S1.11 moved, asserted by
      `the_state_directory_is_the_state_files_own_rule` and the
      `state_path_from` tests, and macOS takes it but for its runtime
      directory (`platform/macos/dirs.rs`, Phase 2, with its own test).
- [>] **D5.2** `README.md` gets a "Where things live" table per platform. Done
      when: reviewed by Brian.
      — port/windows-app (df-app), started 2026-09-29: written b60122c
      (README, "Where things live"): the three columns of §1 as the files'
      homes, the Windows ones as the df-core branch's D5.1 places them, and
      the trash; waits for Brian's review.

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

### 2.2 Windows openers (argv strings, `$1`/`$@`/`$dir`, W4.3, W4.41)

**Enter does what a double-click in Explorer does** (Brian, 2026-10-01):
`builtin:shell-open`, the system's default app, is every file's first
opener but a font's, whose is the shell's `install` verb (W4.43). `O`
offers the alternatives below that this machine has, and every file's list
ends in "Open with…". A row's commands are its candidates, best first: the
first whose program is on `PATH` (as `.exe`, as itself, or as `.cmd`) or
registered in `App Paths` (and then run by the registered path) is the row,
and a row with none of them is not shipped, so no rule offers it (D5.4,
W4.41). The splitter expands no environment variables (W4.3).

| id | candidates | block | description |
|---|---|---|---|
| `open` | `builtin:shell-open` | — | Open |
| `run` | `builtin:shell-open` | — | Run |
| `open-with` | `builtin:shell-open-with` (`SHOpenWithDialog`, Explorer's "Choose another app") | — | Open with… |
| `vscode` | `code "$@"` | no | Open in VS Code |
| `notepad++` | `notepad++ "$@"` | no | Open in Notepad++ |
| `notepad` | `notepad "$1"` | no | Open in Notepad |
| `edit` | `code "$@"`, `notepad++ "$@"`, `notepad "$1"` | no | Edit |
| `paint` | `mspaint "$1"` | no | Edit in Paint |
| `mpv` | `mpv --force-window "$@"` | no | Play in mpv |
| `vlc` | `vlc "$@"` | no | Play in VLC |
| `7-zip` | `7zFM "$1"` | no | Open in 7-Zip |
| `explorer` | `explorer "$1"` | no | Open in Explorer |
| `terminal-here` | `wt -d "$1"`, `cmd /K cd /d "$1"` | no | Terminal here |
| `terminal-at` | `wt -d "$dir"`, `cmd /K cd /d "$dir"` (no rule names it; a `[[open.rules]]` row may) | no | Terminal at file |
| `install-font` | `builtin:font-install` | — | Install |
| `font-viewer` | `builtin:shell-open` | — | Preview in Windows Font Viewer |
| `bulk-rename` | `code --wait "$@"`, `notepad "$@"` | yes | Bulk rename |
| `extract*` | `builtin:*` | — | unchanged |

Gone from Windows' table (W4.41): `zed`, `zed-workspace` (the folder's VS
Code row replaces it), `open-in-chrome` (a browser is `.html`'s default app
already), `play` and `edit-image`.

### 2.3 Rules

macOS uses the Linux `DEFAULT_RULES` table row for row, with `open`
(the system's default app) where a row names `delightviewer`, the other dropped
openers (`delightviewer-edit`, `set-wallpaper`, `optimize-avif`) left out, and
an `open` a row would name twice named once. The `text/*` rule keeps `zed,
edit, open, terminal-at`. (Brian, 2026-09-29: this replaces "minus the rows
that name a dropped opener", which took pictures, video, sound and PDFs down
to the fallback row.)

Windows (Brian, 2026-10-01, W4.41), top-down, each list `O`'s order, the
first what Enter runs:

| match | openers |
|---|---|
| `bulk-rename.txt` | `bulk-rename` |
| `*.{ttf,otf,ttc}` | `install-font`, `font-viewer`, `open-with` |
| `*.{exe,msi}` | `run`, `open-with` |
| `*.{bat,cmd,ps1}` | `run`, `edit`, `open-with` |
| `*.{stl,obj,ply,3mf}` | `open`, `open-with` |
| `*.{gcode,gco}` | `open`, `vscode`, `notepad++`, `notepad`, `open-with` |
| archives, by name and by type | `open`, `extract`, `extract-here`, `extract-merged`, `7-zip`, `open-with` |
| `image/svg+xml` | `open`, `vscode`, `open-with` (Paint cannot read one) |
| `image/*` | `open`, `paint`, `open-with` |
| `video/*`, `audio/*` | `open`, `mpv`, `vlc`, `open-with` |
| `application/pdf` | `open`, `open-with` |
| `text/*`, and JSON, XML, JS, shell, YAML, TOML | `open`, `vscode`, `notepad++`, `notepad`, `open-with` |
| `*/` (a folder's `O`) | `explorer`, `vscode`, `terminal-here` |
| `*` | `open`, `open-with` |

- [x] **D5.3** `platform::defaults::{OPENERS, RULES}` per target; `Config::default`
      (`config.rs`) reads them; the Linux tables are the existing constants
      re-exported from `platform/linux/defaults.rs` so the diff on Linux is a move.
      Done when: `config.rs` tests pass on Linux; a test per target asserts
      `openers_for("x.txt", "text/plain", false)` names `zed` first.
      — (blocked on df-core on the df-app branch) done 29cfb82 on
      `port/macos-finish` (with M2.17 and D5.11), green on the macOS runner (run
      36649716767). Linux's tables are `config.rs`'s constants, re-exported unmoved
      (Decisions log); Windows re-exports the same until W4.3 writes §2.2's;
      macOS has §2.1's openers and §2.3's rules, as §2.3 now reads (ebb95b8).
      `config::tests::a_text_file_opens_in_zed_first_everywhere` is the
      per-target test; the tests that pin Linux's openers and rules run on
      Linux and Windows, and `platform/macos/defaults.rs` pins macOS's.
- [x] **D5.4** `platform::open::first_available(candidates: &[&str]) -> &str` for
      the two-candidate Windows entries (and usable on macOS for `edit`). Done
      when: unit test with a fake `PATH`.
      — nothing to do on the macOS side (df-app, 2026-09-29): the macOS `edit` opener
      carries its fallback in its shell string (§2.1), so nothing there calls
      `first_available`; it is W4.3's, for the Windows argv entries.
      — blocked (df-app, 2026-09-29): `first_available` has no caller until
      §2.2's Windows table exists, and that table is df-core's
      (`platform/windows/defaults.rs`, which re-exports Linux's until then),
      which the df-app branch does not edit; nor does the plan say how one row
      carries two candidates. Options: (a) the row's `run` holds both, one per
      line, and df-app's Windows `spawn_detached` runs the first whose program
      `first_available` finds on `PATH` (no config format change; a user's
      one-line opener is one candidate); (b) the opener gains an optional
      second command in df-core's `Opener` (a config format change, shown in
      the `O` picker as one entry); (c) `Config::default` picks the candidate
      when it builds the table on Windows, through
      `platform::process::candidates` and `PATH`, and no `first_available` is
      written (a program installed while delightfile runs is seen at the next
      start). Until then a Windows config has Linux's openers, which name
      programs Windows does not have.
      — done dbf880a (and c32eaa0, df-app's opener tests), green on the
      Windows runner at run 36666369775, by option (c), as decided (Decisions
      log): `platform/windows/defaults.rs` holds §2.2's table as candidate
      lists, and `platform::defaults::openers()` — `OPENERS` as it stands on
      Linux and macOS — picks each Windows row's first command whose program
      is on `PATH` when `Config::default` builds the table, else its last.
      No `first_available`, no config format change.

## 3. Keymap

Windows: identical to Linux.

macOS (`platform::defaults::KEYMAP_OVERRIDES`, applied after the defaults and
before the user's `keymap.toml`, M2.17):

| Chord (as matched, Cmd = ctrl) | Command | Replaces |
|---|---|---|
| `ctrl+c` (Files) | `copy-to-clipboard` (`Y`) | `close-tab` |
| `ctrl+w` (Files) | `close-tab` | — |
| `ctrl+v` (Files) | `paste` (`p`: the yank, or with nothing yanked the system clipboard) | — |
| `ctrl+c` (Input) | `input-copy` — a new command: copy the field's selection, or the whole text when nothing is selected, to the system clipboard | `overlay-close` |
| `ctrl+c` (Confirm, Pick, Tasks, Spot, Help, Palette) | unchanged `overlay-close` | — |
| `ctrl+q` (Files, where `q` is) | `quit` | — |
| `ctrl+,` | unbound (reserved for a future settings sheet) | — |

Labels (`df_core::platform::keys::LABELS`, M2.21): macOS `⌃` `⌥` `⇧` `⌘`, with the `ctrl`
role rendered `⌘` since Cmd is what people press; a binding the user wrote as
`super+x` in `keymap.toml` is parsed as `ctrl` on macOS (they are the same key
there) with a config warning "super is Cmd, which is Ctrl on macOS". Linux and
Windows: `Ctrl+` `Alt+` `Shift+` `Super+` as today.

- [x] **D5.5** Implement the override table and `input-copy`. (The help sheet
      has no mouse section, so "⌘-click toggles, ⌥-drag links" is not shown
      anywhere: Brian left it out, 2026-09-29.) Done when: a
      `Keymap` test on macOS resolves `ctrl+c` in Files to `yank-to-system`.
      — (blocked on df-core on the df-app branch) done 344715e on
      `port/macos-finish`, green on the macOS runner (run 36649716767), not seen on
      screen: the table is `platform::defaults::KEYMAP_OVERRIDES` (empty on
      Linux and Windows), laid over the shipped keymap before `keymap.toml`,
      each row replacing what the table bound to its keys;
      `platform::defaults::tests::the_cmd_chords_mean_what_a_mac_means` resolves
      `ctrl+c` in Files to `copy-to-clipboard`, the command the plan called
      `yank-to-system`. `input-copy` is a df-core command unbound on Linux and
      Windows, and df-app's prompt runs it before the field sees the key
      (`Prompt::copy_text`). Live check 07-verification.md §4.2.

## 4. External binaries

What each platform is expected to have, and what happens when it is absent (all
absences are already "missing feature, never a failure" on Linux; the table is for
the README and for choosing defaults).

| Binary | Used for | Linux | macOS | Windows |
|---|---|---|---|---|
| `7z` | 7z/rar/iso listing and extraction, `A` to 7z | p7zip | `brew install 7zip` (`7zz`: add `7zz` to `candidates("7z")` on macOS) | 7-Zip installer (`7z.exe` on PATH if the user adds it), or the standalone `7za.exe` (7z, zip, tar, gzip, xz; no rar) |
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
      — the Windows half done 45cff54 (`7za` after `7z`), green on the Windows
      runner at run 36666369775; the macOS half is open.
- [>] **D5.8** README "What it uses" table per platform from §4. Done when:
      reviewed.
      — port/windows-app (df-app), started 2026-09-29: written b60122c
      (README, "What it uses"), from §4 with the df-core branch's `7za`; waits
      for Brian's review.

## 5. Text

- [x] **D5.9** `cli::USAGE` head says "a keyboard-first file manager" (no
      "for Wayland"); `platform::cli::EXTRA_USAGE` carries the `--portal` paragraph
      on Linux (S1.31). Crate descriptions in both `Cargo.toml`s and the `main.rs`
      doc comment updated in the same change. Done when: `--help` output snapshot
      test per target.
      — done 5b43b31 for macOS and Windows: `platform::cli::TITLE_USAGE` is
      the first line, and `the_help_is_the_platforms_own` holds each
      platform's whole `--help`. Linux keeps "for Wayland", as the Phase 4
      brief requires its `--help` unchanged; df-app's crate description names
      the three platforms, the `main.rs` doc comment too (5722b0a); df-core's
      description never named Wayland.
- [x] **D5.10** Help sheet (`help.rs`) and which-key: no text change beyond the
      labels (M2.21). The `?` sheet's footer line naming the config path uses
      `platform::dirs::config_dir()` (verify it does not hard-code `~/.config`).
      Done when: grep finds no `.config/delightfile` literal in df-app.
      — done, no change needed (df-app, 2026-09-29): the help sheet names no
      config path, and `grep -rn "\.config/delightfile" crates/df-app/src`
      finds nothing.

## 6. Bookmarks

| Key | Linux (unchanged) | macOS | Windows (W4.40) |
|---|---|---|---|
| `g h` | `~` | `~` | `%USERPROFILE%`, named Home |
| `g c` | `~/.config` | `~/.config` | the system drive's root, `%SystemDrive%\` (`C:\`) |
| `g a` | `/mnt/schwabserverroot/files/Projects` | — | `%APPDATA%`, named AppData |
| `g d` | `~/Downloads` | `~/Downloads` | Downloads |
| `g w` | `~/Work` | `~/Work` | — |
| `g D` | — | `~/Desktop` | Desktop |
| `g o` | — | `~/Documents` | Documents |
| `g p` | `/mnt/schwabserverroot/plex` | — | Pictures |
| `g v` | — | — | Videos |
| `g m` | — | — | Music |
| `g /` | — | — | the Places card (a keymap row, not a bookmark: `KEYMAP_OVERRIDES`) |
| (others) | `s`, `1`, `2`: `/mnt/schwabserverroot`, `sftp://showandtour1/2` | — | — |

Windows' known folders (Downloads to Music) are where `SHGetKnownFolderPath`
says they are, so one moved or taken into OneDrive is found there, else
under the profile. Each is written as Windows writes it, on the which-key
card as "Downloads · C:\Users\admin\Downloads" and on the Places card by its
name with its path beside it; a folder the machine cannot name is left out.

- [x] **D5.11** `platform::defaults::BOOKMARKS`; Linux is the existing constant
      moved. Done when: `keymap/defaults.rs` reads it and tests pass.
      — (blocked on df-core on the df-app branch) done 29cfb82 on
      `port/macos-finish` (with M2.17 and D5.3), green on the macOS runner (run
      36649716767): `default_bookmarks()`, and through it `keymap/defaults.rs`, reads
      `platform::defaults::BOOKMARKS`. Linux's is `DEFAULT_BOOKMARKS`
      re-exported unmoved (Decisions log). macOS and Windows ship their
      columns above, `g D` and `g o` on Finder's letters as Brian chose
      (f0d0573; a places test that pinned on `o` moved to `q` in 854dd1e);
      Windows' `g c` is `~/AppData/Roaming`, where `%APPDATA%`
      points by default, since a bookmark expands `~` and nothing else.
      `platform/{macos,windows}/defaults.rs` each pin their table.

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
- (macos-finish) 2026-09-29 — D5.3 and D5.11: Linux's tables are not moved
  out of `config.rs` but re-exported from `platform/linux/defaults.rs`, so the
  change on Linux is the read going through the seam and `port/paths`, which
  is editing `config.rs`, meets three lines of it; Windows ships them too
  until W4.3 (02-macos.md Decisions log has the rest, and how §2.3 was read).
- (macos-finish) 2026-09-29 — §3 and §6 corrected to the code: the commands
  are `copy-to-clipboard` (`Y`) and `paste` (`p`, which pastes the system
  clipboard when nothing is yanked), not `yank-to-system` and `paste-system`,
  which were never written; `ctrl+q` is bound in Files, where `q` is; and the
  home bookmark's key is `h` (`g h`), not `~`.
- (macos-finish) 2026-09-29 — D5.5: an override row replaces whatever the
  table bound to its keys in that context (`Registry::unbind`, then
  `register`), which is what `keymap.toml` does with a row of its own.
  `input-copy` copies nothing from an empty field and says nothing, as the
  bulk card's copy does, and otherwise goes out through `App::offer` like
  every copy, with the toast "Copied text". On Linux and Windows it exists
  and is unbound: a `keymap.toml` can bind it.
- 2026-09-29 — Brian-delegated, taken out of the Open questions: §2.3 puts
  `open` where delightviewer was rather than dropping those rows (ebb95b8);
  `g D` is `~/Desktop` and `g o` is `~/Documents` on macOS and Windows,
  Finder's letters, and Windows ships its §6 column in place of Linux's
  table, `%APPDATA%` written `~/AppData/Roaming` (f0d0573); the help
  sheet's "⌘-click toggles, ⌥-drag links" is left out and shown nowhere,
  the sheet having no mouse section (D5.5).
- (df-app) 2026-09-29 — D5.9 keeps "for Wayland" in Linux's `--help`: the
  Phase 4 brief holds Linux's `--help` to what it printed, and on Linux the
  words are true. The first line is `platform::cli::TITLE_USAGE`, macOS and
  Windows saying only "a keyboard-first file manager".
- (df-core) 2026-09-29 — D5.1, Windows: each `platform::dirs` function
  answers the base its callers add a folder to, as on Unix, so the table's
  Windows cells are the files' homes, not the functions' answers:
  `config_dir()` is `%APPDATA%` (the config is `%APPDATA%\delightfile\`),
  `state_dir()` and `data_dir()` are `%LOCALAPPDATA%` (the state file is
  `%LOCALAPPDATA%\delightfile\state`, zoxide's database
  `%LOCALAPPDATA%\zoxide\db.zo`), and `cache_dir()`, which nothing calls, is
  `%LOCALAPPDATA%` as well. An unset or empty variable falls back to its
  folder under `%USERPROFILE%` (`AppData\Roaming`, `AppData\Local`).
  `HOME` is never read there.
- (df-core) 2026-09-29 — D5.1: yazi's `vfs.toml` is found through a new
  `platform::dirs::yazi_config_dir()` — `yazi` under `config_dir()` on Unix,
  which is the path `vfs::config::config_paths` built before, and
  `%APPDATA%\yazi\config` on Windows, yazi's extra level there.
- (df-core) 2026-09-29 — D5.1: the thumbnail cache is not `dirs::cache_dir`
  but `preview::cache::cache_dir`, `%TEMP%` + `yazi-` + the platform's
  suffix, and on Windows that suffix is `0` (yazi's `uid_or_zero`, as
  `platform/windows/user.rs` records from yazi's source), not the
  `<USERNAME>` the table guessed: the Windows half of the open question below
  is answered.
- (df-core) 2026-09-29 — D5.4: option (c), decided for Brian and relayed to
  the df-core branch: no config format change and no `first_available` API.
  `platform::windows::defaults::CANDIDATES` holds §2.2's openers with each
  row's commands in order of preference, and `platform::defaults::openers()`
  (the seam's new entry, which `Config::default` reads in place of the
  `OPENERS` constant) picks per row the first whose program is on `PATH` —
  under the names Windows runs it by, `candidates` and `.cmd`, the form VS
  Code's `code` takes — else the last, which is `notepad` or `cmd`, always
  there. A builtin needs no program. On Linux and macOS `openers()` is
  `OPENERS` as it stands, so their tables are unchanged. §2.2's table is
  written in full, and Windows' rules are Linux's row for row as §2.3 reads
  them, `edit-image` becoming `open`; the tests that pinned Linux's openers
  and rules on Windows too ("until W4.3") now run on Linux alone, and
  `platform/windows/defaults.rs` pins Windows'.
- (df-core) 2026-09-29 — D5.7, Windows: `candidates("7z")` is `7z.exe`, `7z`,
  then `7za.exe`, `7za` — 7-Zip's standalone console build, which takes the
  same commands and switches and reads 7z, zip, tar, gzip and xz but not rar.
  Found after the full `7z`, so a machine with both uses the one that reads
  everything; a machine with only `7za` extracts and writes 7z rather than
  falling to `tar.exe` for it. `tar.exe` for `bsdtar` was W4.2's, in Phase 1.
- (polish) 2026-10-01 — §2.2, §2.3 and §6's Windows columns are Brian's
  2026-10-01 tables (`04-windows.md` §13, W4.40, W4.41): Enter is the
  double-click, `O` what this machine has, the bookmarks a Windows user's
  places. D5.4's candidate rule changes with them: a row none of whose
  programs is here is not shipped, where it shipped its last command; there
  is no last command every machine has once VS Code, Notepad++, VLC and
  7-Zip are rows of their own. A program found in `App Paths` and not on
  `PATH` is run by its registered path, which `start` and Run reach it by and
  `CreateProcess` does not. 05's opener principle is logged in 04's
  Decisions log with the rows' details.
- (polish) 2026-10-01 — `g /` is not a bookmark but a keymap row
  (`KEYMAP_OVERRIDES`, `mount-manager`), Windows' alone: the brief named it
  as Linux's way to the root, but Linux ships no `g /`, and none is added
  there.

## Open questions

- yazi's exact cache-dir suffix on macOS and Windows (verify at D5.1).
  Windows: `0` (Decisions log, 2026-09-29); macOS still to verify.
- Whether `wt` should be the Windows terminal default (see `04-windows.md`):
  it is the first candidate of `terminal-here`, `cmd` the second, so a
  machine without `wt` gets `cmd`.
- Ghostty `-e` on macOS (see `02-macos.md`).
- (df-app) D5.4: how a Windows opener row carries its two candidates, and which
  branch writes §2.2's table into df-core's `platform/windows/defaults.rs`
  (options under D5.4).
