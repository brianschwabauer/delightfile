#!/usr/bin/env bash
# Install delightfile into $HOME — the no-root, no-package path. Everything
# lands under ~/.local, and re-running is safe: files are overwritten in place
# and the databases are rebuilt.
#
#   cargo build --release
#   bash build/install.sh
#
# Uninstall is `rm` of the paths this prints, then the same two database
# refreshes.
#
# Two things this deliberately does *not* do, because they are the machine's
# opinion rather than the program's:
#
#   * make delightfile the default handler for `inode/directory` — that is one
#     `xdg-mime default delightfile.desktop inode/directory`, printed at the
#     end so it is a copyable decision rather than a surprise;
#   * point `xdg-desktop-portal-termfilechooser` at the wrapper it installs —
#     also printed, for the same reason.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo/target/release/delightfile"

data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
bindir="$HOME/.local/bin"

if [[ ! -x $bin ]]; then
    echo "no release binary at $bin — run: cargo build --release" >&2
    exit 1
fi

install -Dm755 "$bin" "$bindir/delightfile"
# The basename has to stay `delightfile.desktop`: it is the Wayland app_id the
# window sets, and that match is what pairs the window with the icon.
install -Dm644 "$repo/build/delightfile.desktop" \
    "$data/applications/delightfile.desktop"
install -Dm644 "$repo/build/delightfile.svg" \
    "$data/icons/hicolor/scalable/apps/delightfile.svg"
# …and the same icon as bitmaps, because some launchers and taskbars only look
# in the sized directories and leave the scalable one alone. Checked in rather
# than rendered here: three files under 5 KB beat a dependency on a rasteriser
# at install time.
for size in 48 128 256; do
    install -Dm644 "$repo/build/icons/delightfile-$size.png" \
        "$data/icons/hicolor/${size}x${size}/apps/delightfile.png"
done
# The portal wrapper goes in the portal's *own* config directory, which is the
# first place its modified PATH looks — so `cmd=delightfile-wrapper.sh` needs
# no absolute path and keeps working if this repo moves.
install -Dm755 "$repo/build/delightfile-wrapper.sh" \
    "$config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh"

echo "installed:"
echo "  $bindir/delightfile"
echo "  $data/applications/delightfile.desktop"
echo "  $data/icons/hicolor/scalable/apps/delightfile.svg"
echo "  $data/icons/hicolor/{48x48,128x128,256x256}/apps/delightfile.png"
echo "  $config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh"

# Each of these is optional — a machine without it just keeps a slightly stale
# database until something else rebuilds it.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data/applications"
    echo "refreshed desktop database"
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
fi

case ":$PATH:" in
*":$bindir:"*) ;;
*) echo "note: $bindir is not on PATH" ;;
esac

cat <<'NEXT'

to make it the desktop's file manager:
  xdg-mime default delightfile.desktop inode/directory

to make it the file picker every GTK/Qt dialog goes through, install
xdg-desktop-portal-termfilechooser and write these two files:

  ~/.config/xdg-desktop-portal-termfilechooser/config
      [filechooser]
      cmd=delightfile-wrapper.sh
      default_dir=$HOME
      open_mode=suggested
      save_mode=suggested

  ~/.config/xdg-desktop-portal/portals.conf
      [preferred]
      org.freedesktop.impl.portal.FileChooser=termfilechooser

then: systemctl --user restart xdg-desktop-portal
NEXT
