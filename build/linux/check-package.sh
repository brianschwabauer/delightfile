#!/usr/bin/env bash
# B6.14's acceptance check for the Linux tarball: unpack it somewhere new and
# run its install.sh into an empty home, the way someone who downloaded it
# would, then see that the binary it installed runs and that the install said
# what to do next.
#
#   bash build/linux/check-package.sh delightfile-<version>-x86_64-linux.tar.gz
#
# Meant for a clean machine, which is how the release workflow runs it: a
# fresh archlinux container with only FFmpeg added, no session bus, no
# xdg-desktop-portal, nobody at a terminal. Nothing outside a temporary
# folder is touched; HOME and the XDG variables point into it.
set -euo pipefail

tarball="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
name="$(basename "$tarball" .tar.gz)"
version="${name#delightfile-}"
version="${version%-x86_64-linux}"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/unpacked" "$scratch/home"
tar -xzf "$tarball" -C "$scratch/unpacked"

export HOME="$scratch/home"
unset XDG_DATA_HOME XDG_CONFIG_HOME XDG_CURRENT_DESKTOP DBUS_SESSION_BUS_ADDRESS
log="$scratch/install.log"
(cd "$scratch/unpacked/$name" && bash install.sh </dev/null) 2>&1 | tee "$log"

fail() {
    echo "check-package: $*" >&2
    exit 1
}

said="$("$HOME/.local/bin/delightfile" --version)"
[[ $said == "delightfile $version" ]] || fail "the installed binary says '$said', not 'delightfile $version'"
for file in \
    .local/share/applications/delightfile.desktop \
    .local/share/icons/hicolor/scalable/apps/delightfile.svg \
    .local/share/icons/hicolor/48x48/apps/delightfile.png \
    .local/share/icons/hicolor/128x128/apps/delightfile.png \
    .local/share/icons/hicolor/256x256/apps/delightfile.png \
    .local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.delightfile.service \
    .local/share/dbus-1/services/org.freedesktop.FileManager1.service \
    .local/share/xdg-desktop-portal/portals/delightfile.portal \
    .config/xdg-desktop-portal/portals.conf \
    .config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh; do
    [[ -f $HOME/$file ]] || fail "install.sh did not install ~/$file"
done
grep -q "Exec=$HOME/.local/bin/delightfile" \
    "$HOME/.local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.delightfile.service" ||
    fail "the portal's service file does not start ~/.local/bin/delightfile"
grep -qx 'then: systemctl --user restart xdg-desktop-portal' "$log" ||
    fail "install.sh did not print the portal-restart note"

(cd "$scratch/unpacked/$name" && bash install.sh uninstall </dev/null) >/dev/null
[[ ! -e $HOME/.local/bin/delightfile ]] || fail "uninstall left ~/.local/bin/delightfile"

echo "check-package: $name installs from its own folder, runs ($said), prints the restart note and uninstalls"
