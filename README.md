# delightfile

A keyboard-first file manager for Wayland, written in Rust and drawn on the GPU.

It started as a replacement for [yazi](https://github.com/sxyazi/yazi) and keeps yazi's
keymap almost key for key, so the muscle memory carries over. What it adds is everything a
terminal cannot do: real drag-and-drop into other applications, decoded video and audio in
the preview pane with shuttle controls, undo for every destructive operation, and a mouse
that behaves like one.

![The list view, with two tabs and an image preview](docs/screenshot-list.png)

`=` steps up a ladder of four densities. At the top, the same directory is a thumbnail grid.
Each tab keeps its own step as it moves between folders, and a new tab starts at
`[mgr] view_scale`.

![The grid view](docs/screenshot-grid.png)

## Status

Every phase of [PLAN.md](PLAN.md) is built. About 129,000 lines of Rust and 1,531 tests,
clippy-clean. The workspace warns on `unsafe_code`, so each of the sixty-odd uses is opted
in by hand and sits next to a comment saying why: libc calls for inotify, `statfs`,
`copy_file_range` and thread priority, the parent-death signal that stops an rclone daemon
outliving the app, the Wayland data-device client, and FFmpeg's FFI in the vendored
`dv-media`.

It is my daily file manager on Hyprland with an NVIDIA card. It has not been tested on
other compositors, other GPUs, or X11. Bugs from those are expected rather than
surprising.

### Cross-platform

The port to macOS and Windows is planned, phase by phase, in
[plans/other-platforms/README.md](plans/other-platforms/README.md), which is also where its
progress is marked. The first phase, cutting the code along a platform seam so that one
source tree builds for all three, is under way; neither port has been run yet.

## What is in it

Three miller columns, a preview pane that decodes rather than shells out, and a top bar
that carries the breadcrumbs, the position counter, the git branch, and whatever prompt is
open.

- **Previews that decode.** Images including AVIF and HEIF, video with a scrub strip and
  the `j`/`k`/`l` shuttle ladder, audio with a waveform, PDF pages, font specimen sheets,
  3D models on a turntable, G-code layers, syntax-highlighted text, rendered markdown, and
  archive contents. Animated GIF, WebP, APNG and AVIF play in place.
- **Drag and drop.** Drag rows out to Chrome's upload box or to an editor over
  `text/uri-list`. Drop files in from anywhere. Drag between panes, tabs and windows, with
  a stacked-card ghost and a count badge.
- **Undo.** Every move, copy, rename, trash, link, tag and permission change goes through a
  journal, and `u` walks it back. `U` walks forward again, checking first that nothing has
  changed underneath, and the app menu's Edit ▸ Undo history… lists every step either way
  and walks to any of them.
- **Permissions.** `C` sets a selection's bits as boxes or a typed `755`, inside folders
  too, and `u` puts them back.
- **Tags.** `T` tags a file or a selection, stored in the freedesktop `user.xdg.tags`
  extended attribute, so the tags live on the file and other programs can read them. The
  seven Finder colours (and any tag `[tags]` gives a colour) show as overlapping dots after
  the name. `f #red` filters a folder by tag and `s #red` finds tagged files anywhere
  below it. Copies, moves across drives and the trash carry the tags along, and so does
  a sync for every file it copies (one whose only change is its tags is not copied again);
  a drive that cannot hold them, like a FAT card, gets the files without them and says so.
  Tags are read on local drives only, not over NFS, SMB or FUSE mounts.
- **Git awareness.** Status dots per row, dimmed hidden files, branch and dirty count in
  the top bar.
- **Disk usage.** A background `du` streams folder sizes into the size column and settles
  them as it goes. No ncdu round trip.
- **Archives as directories.** Walk into zip, tar, tar.gz, tar.xz and tar.zst, preview
  what is inside, extract the selection. 7z, rar and bzip2 are detected and refused rather
  than half-read.
- **Places.** `M` lists everywhere that is not this folder, one line each: the disks
  udisks2 knows (mounted or not, over a hand-rolled D-Bus client) and the phones and
  cameras gvfs has seen, then gvfs's network shares (SMB, SFTP, FTP, WebDAV, NFS), the
  rclone remotes and a connect prompt, then your pins and `[goto]` rows with their keys.
  `Enter` mounts what is not mounted and goes into what is.

  A phone needs gvfs-mtp (a camera, gvfs-gphoto2), and it has to be unlocked and set to
  File transfer before it will open. Plugged in while the window is up, it gets a toast
  naming the key. Inside one, folder sizes are not walked and the grid shows icons, so
  browsing a phone does not pull every photo over USB.
- **Trash.** The freedesktop trash browsed as a directory with restore on `Enter`.

  A chip beside the position counter weighs the trash (`37 items · 1.2 GB`, counted in
  the background like the size column), and "Empty trash" asks with the same numbers.
  Anything in the home trash longer than `trash_keep_days` (30 by default) is removed for
  good, at most once a day however many windows are open: a stamp file in the trash
  (`.delightfile-purge`) records the last purge, and a lock on it keeps two windows from
  purging at once. It runs as a task you can cancel from `w`, waits while the trash is
  open in a tab (opening the trash stops one that is running), never runs from a file
  dialog, and never touches an item whose deletion date it cannot read.
- **SFTP.** Hosts from yazi's `vfs.toml` browse as directories, with download-on-open and
  upload-on-drop.
- **Cloud storage.** Every remote in `rclone.conf` (Google Drive, Dropbox, S3, R2, anything
  rclone speaks) browses the same way at `rclone://<name>`. rclone does the talking, so
  delightfile never holds a token.
- **Search.** `s` searches names with `fd` and `S` searches contents with `rg`, streamed
  into a panel with the preview following the highlighted hit. `Enter` turns the hits into
  the tab's listing, named by their path from where you searched, so select, yank, trash,
  rename and bulk rename, drag out, sort and archive all work on files from all over the
  tree; a content search's column shows each file's first matching line. `Alt+Enter` goes
  to one hit's folder instead, and `←` goes back. `Enter` on an empty name search lists
  everything below the folder as one flat list.
- **Tabs.** Folder tabs joined to the bar below them, reorderable by dragging, and
  detachable into their own window.
- **Light and dark.** The window follows the desktop's light or dark preference through
  the XDG desktop portal and turns the moment it changes: catppuccin-mocha on the dark
  side, catppuccin-latte on the light. The app menu's Appearance list holds one side for
  the session instead.

It shares a thumbnail cache with yazi and with
[delightviewer](https://github.com/brianschwabauer/delightviewer), so all three hand off
the exact frame you were looking at.

## Building

Linux, in a Wayland session, is the platform delightfile supports. macOS and Windows are a
port in progress: their builds are meant to compile, with stand-ins that say "not
available on this platform" where a feature has no native body yet, but they are
unverified and nobody has run one. The plan and how far it has got are in
[plans/other-platforms/](plans/other-platforms/README.md).

On Linux it needs a recent stable Rust and FFmpeg 9 development libraries, which
`ffmpeg-next` links against. On Arch that is `pacman -S rust ffmpeg`.

```sh
cargo build --release
```

The binary lands at `target/release/delightfile`. `bash build/install.sh` copies it to
`~/.local/bin` along with the desktop entry and the icons, registers it as the desktop's
file-picker backend (see below), and prints the command that makes it the default file
manager. `bash build/install.sh uninstall` takes all of it back out.

PDF previews want `libpdfium.so`, loaded at runtime from `~/.local/lib/delightfile/`,
`~/.local/lib/delightviewer/`, or the system loader. Grab a build from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (ABI
`pdfium_7881`). Without it a PDF falls back to its thumbnail, which is a missing feature
and never an error.

Row icons are a Nerd Font's glyphs when one is installed, and `ls`-style classifiers when
none is. On Linux any patched face in the system or user font directories is found (on
Arch, `pacman -S ttf-jetbrains-mono-nerd`); on macOS `brew install --cask
font-symbols-only-nerd-font` puts one in `~/Library/Fonts`, where it is looked for.

`fd`, `rg`, `git`, `zip`, `bsdtar` and `wl-copy` are run as subprocesses when they exist.
Each one only powers its own feature, and a missing binary shows up as a message naming it
rather than as a failure. The trash, the D-Bus mount client, the TOML parser and the
clipboard are all implemented here rather than shelled out to.

## Making it the desktop's file manager

```sh
xdg-mime default delightfile.desktop inode/directory
```

That covers `xdg-open`, "open containing folder", and every launcher that asks the desktop
database who handles a directory.

A browser's "Show in folder" goes another way: it calls `org.freedesktop.FileManager1` on
the session bus, and whichever program owns that name opens the folder. Nautilus installs
a service file for the name, so on a machine that has Nautilus, Nautilus answers, and when
it is slow the browser waits until the call times out. `delightfile --portal`, the
file-picker backend below, serves that name as well: `ShowItems` opens the folder each
file is in with the cursor on the file, `ShowFolders` opens each folder, and
`build/install.sh` puts in the service file that makes D-Bus start delightfile for it.
Where another file manager already owns the name, delightfile waits in line and takes it
over when that one exits.

`delightfile --reveal <path>` does the same from a shell: it opens the folder the path is
in with the cursor on it, even when the path is a folder itself.

## Making it the file picker

Every GTK, Qt and browser file dialog on a Wayland desktop goes through
[xdg-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/), which hands it to a
backend. delightfile is one: `delightfile --portal` serves
`org.freedesktop.impl.portal.FileChooser` on the session bus, so Chrome's upload box, its
"Save as", and every "Attach a file" in every web app open a delightfile window instead of
GTK's dialog — told the dialog's title, the caller's button label ("Upload" rather than
"Select"), the suggested file name, the folder to start in, and the file-type filters.

`build/install.sh` sets it up for the user running it, no root needed:

- a D-Bus service file, `~/.local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.delightfile.service`,
  so the bus starts the backend on the first dialog;
- a second one, `~/.local/share/dbus-1/services/org.freedesktop.FileManager1.service`,
  which starts the same backend for "Show in folder" (see above) and takes precedence over
  the one another file manager put in `/usr/share`;
- `~/.local/share/xdg-desktop-portal/portals/delightfile.portal`, which xdg-desktop-portal
  reads from there since 1.20.1 (on an older one the script installs it to
  `/usr/share/xdg-desktop-portal/portals/` with `sudo`, or prints the command);
- the preference, in `~/.config/xdg-desktop-portal/portals.conf`:

  ```ini
  [preferred]
  org.freedesktop.impl.portal.FileChooser=delightfile
  ```

  If that file already names a different file chooser, it is left alone and the line to
  change is printed instead.

Then `systemctl --user restart xdg-desktop-portal`. `bash build/install.sh --remove-portal`
undoes all four.

The picker window's Wayland app_id (X11 class) is `delightfile-picker`, not `delightfile`,
so a window rule can float and centre it without touching the file manager. Its title is
whatever the calling program called the dialog.

In a picker session the top row ends with `Cancel` and a button that answers the dialog:
`Select` for files, `Choose folder` for a folder, `Save` for a save — or the calling
program's own word, such as `Upload` — and `Ctrl+Enter` presses it. `Enter` picks the
selection (or the file under the cursor) and quits, a directory still opens on `Enter`, a
double-click picks the row it lands on, and `q`, `Esc` or `Cancel` cancels. A dialog that
takes one file holds the selection to one row, and a drag draws no band there. In a save,
`Enter` on a file replaces it (asking first unless it is the name the dialog suggested) and
`Save` types a new name.

When the dialog comes with file-type filters (a web form that accepts only images, say),
the listing shows only what the active filter admits; folders always show. A chip beside
the position counter names the filter, and clicking it — or `F10` → File type — switches
to another filter the dialog offered, or to all files. The `.` key, which normally toggles
hidden files, is a three-step ladder in such a dialog: the filter's files, then every file,
then every file including the hidden ones, and round again. Narrowing the view deselects
anything it hides, so a pick never includes a file you cannot see.

### The termfilechooser alternative

[xdg-desktop-portal-termfilechooser](https://github.com/hunkyburrito/xdg-desktop-portal-termfilechooser)
is a backend that hands a wrapper script an output path and runs a file manager against it.
`build/delightfile-wrapper.sh` is that wrapper, and the installer puts it where the portal
looks. It works, but the wrapper is only told whether the dialog takes several files, a
folder, or a save — the title, the button label and the filters do not reach the window. To
use it instead, write these two files:

```ini
# ~/.config/xdg-desktop-portal-termfilechooser/config
[filechooser]
cmd=delightfile-wrapper.sh
default_dir=$HOME
open_mode=suggested
save_mode=suggested
```

```ini
# ~/.config/xdg-desktop-portal/portals.conf
[preferred]
org.freedesktop.impl.portal.FileChooser=termfilechooser
```

A window the wrapper starts has the title `file-picker`.

### Underneath

The mechanism underneath is one flag, plus three switches the termfilechooser wrapper
passes through from the portal:

```sh
delightfile --chooser-file=/tmp/picked ~/Downloads
delightfile --chooser-file=/tmp/picked --chooser-multiple ~/Downloads
delightfile --chooser-file=/tmp/picked --chooser-directory ~/Downloads
delightfile --chooser-file=/tmp/picked --chooser-save ~/Downloads/photo.jpg
```

Whatever was picked is written to that file, one absolute path per line. Nothing is written
if you quit any other way, which is how the portal is told the dialog was dismissed. The
backend passes `--chooser-request=<file>` instead of the switches: a short TOML file with
everything the portal said about the dialog, written for each dialog into
`$XDG_RUNTIME_DIR/delightfile/` and removed once it has been answered.

## Keys

Arrow keys navigate. `hjkl` does not, and that is the one deliberate break from yazi:
`j`, `k` and `l` are the video transport, globally, so you can arrow onto a video and play
it without leaving the list.

| | |
|---|---|
| `↑` `↓` `←` `→` | move, leave, enter |
| `g g` `G` | top, bottom |
| `Ctrl+u` `Ctrl+d` | half page |
| `Alt+←` `Alt+→` | history back, forward |
| `Ctrl+l` | type a path to go to |
| `Enter` `o` | open. `O` picks the opener |
| `Alt+Enter` | go to a search hit's folder, the cursor on it |
| `Ctrl+t` | open a terminal here (the `terminal-here` opener) |
| `Ctrl+Enter` | in a file dialog: its Select / Choose folder / Save button |
| `Space` `v` `Ctrl+a` | select, visual mode, all |
| `y` `x` `p` `P` | yank, cut, paste, paste over |
| `Y` `c t` | copy the file to the clipboard, copy its text |
| `d` `D` | trash, delete for good |
| `a` `r` `R` | create, rename, rename the stem |
| `C` | permissions: the selection's nine bits, or everything inside a folder |
| `T` | tags, comma-separated. `Tab` completes one |
| `u` `U` | undo, redo |
| `f` `/` `n` `N` | filter, find, next, previous. `f #red` filters by tag |
| `s` `S` | search names (`fd`), search contents (`rg`). `s #red` searches by tag |
| `m t` | tag names in the right-hand column (`m` picks what the column says) |
| `z` `Z` | fuzzy jump, zoxide jump |
| `t` `1`–`9` `Alt+[` `Alt+]` | new tab, switch, previous, next |
| `Tab` | the spot panel: metadata, EXIF, checksum |
| `-` `=` | walk this tab's density ladder: Compact, Comfortable, Roomy, Grid |
| `j` `k` `l` | video shuttle, anywhere |
| `Ctrl+←` `Ctrl+→` | frame step or page turn in the preview |
| `M` `w` `g t` | places (drives, phones, network, pins), tasks, trash |
| `Ctrl+p` | command palette |
| `F10` | app menu, also the `≡` button at the left of the path |
| `?` | help |
| `q` `Q` | quit. `Q` skips the cwd-file |

Hold a prefix key and a which-key card lists what follows it.

## Configuration

`~/.config/delightfile/delightfile.toml`, `keymap.toml` and `theme.toml`. All three are
optional. The defaults are a port of my yazi config, so they carry its opener rules (with
delightfile's own edits), the same `[1, 4, 3]` column ratio, catppuccin-mocha, and the
nineteen custom directory icons. The one thing yazi never had is a light side, and that
follows the desktop.

`theme.toml` picks the flavour for each side and overrides single colours, on both sides
or on one:

```toml
[flavor]
mode = "auto"               # auto (follow the desktop), dark or light
dark = "catppuccin-mocha"   # catppuccin-mocha, -macchiato, -frappe or -latte
light = "catppuccin-latte"

[palette]                   # both sides
blue = "#8aadf4"

[palette.dark]              # the dark side only
base = "#1a1a28"

[palette.light]             # the light side only
base = "#f4f5f8"
```

A side's own table wins over `[palette]`, wherever either is written in the file. `auto`
asks the desktop portal for `org.freedesktop.appearance color-scheme` and keeps listening;
no preference, or no portal, is dark. `theme-auto`, `theme-dark` and `theme-light` (the
Appearance list in the app menu, unbound by default) change the side for the session
without touching the file.

`[mgr] trash_keep_days` is how many days the trash keeps things before they go for good,
counted from the deletion date each item's record carries. It defaults to 30, and `0`
keeps everything until the trash is emptied by hand.

```toml
[mgr]
trash_keep_days = 30
```

Tags named `red`, `orange`, `yellow`, `green`, `blue`, `purple` and `grey` are coloured
with the theme's colours of those names. `[tags]` in `delightfile.toml` colours any other
tag, by palette name or hex. A palette name is that colour on whichever side is showing,
so it turns with the window; a hex is the same on both:

```toml
[tags]
work = "blue"
urgent = "#ff0000"
"invoice 2026" = "green"
```

SFTP hosts come from `~/.config/yazi/vfs.toml` first and
`~/.config/delightfile/vfs.toml` second, so an existing yazi setup needs no second copy.

Cloud remotes come from rclone's own config (`$RCLONE_CONFIG`, else
`~/.config/rclone/rclone.conf`). Each `[section]` there is a service of the same name,
listed after the `vfs.toml` ones and under Network on the `M` card. To start inside a
bucket, or to reach a remote under another name, add it to `vfs.toml`:

```toml
[services.photos]
type = "rclone"
remote = "r2"           # the section in rclone.conf; defaults to the service name
root = "photos-bucket"  # optional
```

`rclone://photos` then opens `r2:photos-bucket`. An encrypted `rclone.conf` cannot be read
for its remotes, so list them in `vfs.toml` instead; rclone itself takes the password from
`RCLONE_CONFIG_PASS`.

`--cwd-file=<path>` writes the directory you ended in when you quit with `q`, which is what
lets a shell function follow you:

```sh
df() {
  tmp=$(mktemp)
  delightfile --cwd-file="$tmp" "$@"
  cd "$(cat "$tmp")" || return
  rm -f "$tmp"
}
```

## Architecture

Five crates:

- `df-core` is headless. The filesystem model, sorting and filtering, the operation journal
  and undo, the keymap engine, config parsing, the task engine, previews, git, archives,
  `du`, zoxide, and the SFTP and rclone vfs. It never touches winit, egui or wgpu, so
  `cargo test` exercises it on a machine with no display.
- `df-app` is the binary. One winit `ApplicationHandler`, every pixel painted onto an
  `egui::Painter`, and the parts that need a compositor: drag-and-drop, the clipboard,
  D-Bus.
- `dv-core`, `dv-media` and `dv-playback` come from delightvideo by way of delightviewer,
  vendored unchanged so this repository builds on its own.

Two decisions shape the rest. The event loop repaints on events and never polls, so an
idle window costs zero frames and `DF_FRAME_LOG=1` names whatever woke it. And motion is
pure functions over `(inputs, Instant)` with tests, so no animation was eyeballed.

There are no widgets. Everything is drawn straight onto a painter, which is why the hover,
ripple and press behaviour is consistent across rows, chips, tabs and menus: there is one
implementation of each.

## Non-goals

No Lua or plugin scripting. No embedded terminal. X11 only if it falls out of winit for
free.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

The vendored `dv-*` crates arrived under that license and stay under it. Separately, any
binary built here links FFmpeg, and a distribution's FFmpeg is usually compiled
`--enable-gpl`, so the compiled result has to be redistributed under the GPL regardless of
what this repository says.
