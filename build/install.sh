#!/usr/bin/env bash
# Install delightfile into $HOME — the no-root, no-package path. Everything
# lands under ~/.local (and ~/.config for the portal preference), and
# re-running is safe: files are overwritten in place and the databases are
# rebuilt.
#
#   cargo build --release
#   bash build/install.sh                  # install, portal backend included
#   bash build/install.sh --remove-portal  # undo just the portal backend
#   bash build/install.sh uninstall        # remove everything this installs
#
# The file picker is the program's own business, so it is wired up here:
# delightfile is its own xdg-desktop-portal backend (`delightfile --portal`),
# D-Bus starts it on the first file dialog, and portals.conf is pointed at it —
# unless portals.conf already names another file chooser, which is the
# person's choice and is left alone (the line to change is printed instead).
#
# One thing this deliberately does *not* do, because it is the machine's
# opinion rather than the program's: make delightfile the default handler for
# `inode/directory`. That is one `xdg-mime default delightfile.desktop
# inode/directory`, printed at the end so it is a copyable decision rather than
# a surprise.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo/target/release/delightfile"

data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
bindir="$HOME/.local/bin"

bus_name="org.freedesktop.impl.portal.desktop.delightfile"
service="$data/dbus-1/services/$bus_name.service"
portal_user="$data/xdg-desktop-portal/portals/delightfile.portal"
portal_system="/usr/share/xdg-desktop-portal/portals/delightfile.portal"
portals_conf="$config/xdg-desktop-portal/portals.conf"
chooser_key="org.freedesktop.impl.portal.FileChooser"

# ── The portal backend ──────────────────────────────────────────────────────

# The installed xdg-desktop-portal's version, or nothing if it cannot be
# found. DELIGHTFILE_XDP_VERSION stands in for it when testing this script.
xdp_version() {
    if [[ -n ${DELIGHTFILE_XDP_VERSION:-} ]]; then
        echo "$DELIGHTFILE_XDP_VERSION"
        return
    fi
    local exe
    for exe in /usr/lib/xdg-desktop-portal /usr/libexec/xdg-desktop-portal \
        /usr/lib/*/xdg-desktop-portal /usr/local/libexec/xdg-desktop-portal; do
        if [[ -x $exe ]]; then
            "$exe" --version 2>/dev/null | awk '{ print $2; exit }'
            return
        fi
    done
}

# Whether version $1 is at least $2.
version_at_least() {
    [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" == "$2" ]]
}

# Run a command as root if a person is there to type the password, else say
# exactly what to run.
as_root() {
    if [[ -t 0 ]] && command -v sudo >/dev/null 2>&1; then
        echo "running: sudo $*"
        sudo "$@"
    else
        echo "run this as root:"
        printf '  sudo'
        printf ' %q' "$@"
        printf '\n'
    fi
}

# The awk that reads portals.conf a line at a time: which section a line is
# in, and a `key = value` line's two halves, trimmed. Compared as strings, not
# patterns, because the key is full of dots.
ini_awk='
    function trim(text) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", text)
        return text
    }
    {
        name = ""
        value = ""
        if ($0 ~ /^[[:space:]]*\[/) {
            section = trim($0)
        } else if (index($0, "=") > 0) {
            name = trim(substr($0, 1, index($0, "=") - 1))
            value = trim(substr($0, index($0, "=") + 1))
        }
    }
'

# What portals.conf's [preferred] section says for the file chooser: nothing
# when the key is absent, `=<value>` when it is there (so an empty value is
# still "there").
preferred_chooser() {
    [[ -f $portals_conf ]] || return 0
    awk -v key="$chooser_key" "$ini_awk"'
        section == "[preferred]" && name == key { print "=" value; exit }
    ' "$portals_conf"
}

# Point portals.conf at delightfile, creating the file or the section, and
# never overwriting a different choice.
prefer_delightfile() {
    local current tmp
    current="$(preferred_chooser)"
    if [[ $current == "=delightfile" ]]; then
        echo "portals.conf already prefers delightfile for the file chooser"
        return
    fi
    if [[ -n $current ]]; then
        echo "portals.conf prefers '${current#=}' for the file chooser — left alone."
        echo "to use delightfile, change that line in $portals_conf to:"
        echo "  $chooser_key=delightfile"
        return
    fi
    mkdir -p "$(dirname "$portals_conf")"
    if [[ ! -e $portals_conf ]]; then
        printf '[preferred]\n%s=delightfile\n' "$chooser_key" >"$portals_conf"
    elif grep -Eq '^[[:space:]]*\[preferred\][[:space:]]*$' "$portals_conf"; then
        tmp="$(mktemp)"
        awk -v line="$chooser_key=delightfile" '
            { print }
            !done && /^[[:space:]]*\[preferred\][[:space:]]*$/ { print line; done = 1 }
        ' "$portals_conf" >"$tmp"
        # Written through rather than moved over, so a portals.conf that is a
        # symlink into a dotfiles repo stays one.
        cat "$tmp" >"$portals_conf"
        rm -f "$tmp"
    else
        printf '\n[preferred]\n%s=delightfile\n' "$chooser_key" >>"$portals_conf"
    fi
    echo "set $chooser_key=delightfile in $portals_conf"

    # A desktop-specific file in the same directory is read *instead of*
    # portals.conf (portals.conf(5)), so the line above would do nothing.
    local desktop
    IFS=: read -ra desktops <<<"${XDG_CURRENT_DESKTOP:-}"
    for desktop in "${desktops[@]}"; do
        local specific
        specific="$(dirname "$portals_conf")/${desktop,,}-portals.conf"
        if [[ -e $specific ]]; then
            echo "note: $specific is read instead of portals.conf on this desktop;"
            echo "      put $chooser_key=delightfile under [preferred] there too"
        fi
    done
}

# Take delightfile back out of portals.conf — only the line this script
# writes, only when it still says delightfile.
unprefer_delightfile() {
    local tmp
    [[ $(preferred_chooser) == "=delightfile" ]] || return 0
    tmp="$(mktemp)"
    awk -v key="$chooser_key" "$ini_awk"'
        section == "[preferred]" && name == key && value == "delightfile" { next }
        { print }
    ' "$portals_conf" >"$tmp"
    cat "$tmp" >"$portals_conf"
    rm -f "$tmp"
    echo "removed $chooser_key=delightfile from $portals_conf"
}

# Ask the session bus to re-read its service directories, so the first dialog
# can start the backend without a re-login. dbus-daemon notices on its own;
# dbus-broker needs asking. Quiet and optional.
reload_bus() {
    if command -v busctl >/dev/null 2>&1; then
        busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true
    fi
}

install_portal() {
    local tmp version
    tmp="$(mktemp)"
    sed "s|@BIN@|$bindir/delightfile|" \
        "$repo/build/$bus_name.service.in" >"$tmp"
    install -Dm644 "$tmp" "$service"
    rm -f "$tmp"
    echo "  $service"

    # xdg-desktop-portal reads backends from $XDG_DATA_HOME and
    # $XDG_DATA_DIRS since 1.20.1; before that, only from its own
    # /usr/share. The user copy goes in either way — it costs nothing, and
    # it is the one that counts after an upgrade.
    install -Dm644 "$repo/build/delightfile.portal" "$portal_user"
    echo "  $portal_user"
    version="$(xdp_version || true)"
    if [[ -z $version ]]; then
        echo "note: xdg-desktop-portal was not found. Versions before 1.20.1 read"
        echo "      backends only from /usr/share; on one of those, also run:"
        echo "  sudo install -Dm644 $repo/build/delightfile.portal $portal_system"
    elif ! version_at_least "$version" 1.20.1; then
        echo "xdg-desktop-portal $version reads backends only from /usr/share:"
        as_root install -Dm644 "$repo/build/delightfile.portal" "$portal_system"
    fi
    prefer_delightfile
    reload_bus
}

remove_portal() {
    rm -f "$service" "$portal_user"
    echo "removed $service"
    echo "removed $portal_user"
    if [[ -e $portal_system ]]; then
        as_root rm -f "$portal_system"
    fi
    unprefer_delightfile
    reload_bus
}

restart_note() {
    echo
    echo "then: systemctl --user restart xdg-desktop-portal"
}

# ── Everything else ─────────────────────────────────────────────────────────

installed_files=(
    "$bindir/delightfile"
    "$data/applications/delightfile.desktop"
    "$data/icons/hicolor/scalable/apps/delightfile.svg"
    "$data/icons/hicolor/48x48/apps/delightfile.png"
    "$data/icons/hicolor/128x128/apps/delightfile.png"
    "$data/icons/hicolor/256x256/apps/delightfile.png"
    "$config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh"
)

refresh_databases() {
    # Each of these is optional — a machine without it just keeps a slightly
    # stale database until something else rebuilds it.
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data/applications" 2>/dev/null || true
        echo "refreshed desktop database"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
    fi
}

case "${1:-}" in
--remove-portal)
    remove_portal
    restart_note
    exit 0
    ;;
uninstall)
    remove_portal
    rm -f "${installed_files[@]}"
    for file in "${installed_files[@]}"; do
        echo "removed $file"
    done
    refresh_databases
    restart_note
    exit 0
    ;;
"") ;;
*)
    echo "usage: bash build/install.sh [--remove-portal | uninstall]" >&2
    exit 2
    ;;
esac

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
# The termfilechooser wrapper, for machines that route the file chooser
# through xdg-desktop-portal-termfilechooser instead. It goes in that portal's
# *own* config directory, the first place its modified PATH looks — so
# `cmd=delightfile-wrapper.sh` needs no absolute path and keeps working if this
# repo moves.
install -Dm755 "$repo/build/delightfile-wrapper.sh" \
    "$config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh"

echo "installed:"
echo "  $bindir/delightfile"
echo "  $data/applications/delightfile.desktop"
echo "  $data/icons/hicolor/scalable/apps/delightfile.svg"
echo "  $data/icons/hicolor/{48x48,128x128,256x256}/apps/delightfile.png"
echo "  $config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh"
install_portal
refresh_databases

case ":$PATH:" in
*":$bindir:"*) ;;
*) echo "note: $bindir is not on PATH" ;;
esac

cat <<'NEXT'

to make it the desktop's file manager:
  xdg-mime default delightfile.desktop inode/directory

file dialogs (Chrome's upload box, every "Save as") open delightfile once
xdg-desktop-portal is restarted. Its window's Wayland app_id is
`delightfile-picker`, for a window rule that floats it.
NEXT
restart_note
