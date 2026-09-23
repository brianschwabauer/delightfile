#!/usr/bin/env sh
# The wrapper `xdg-desktop-portal-termfilechooser` runs to put delightfile
# where a GTK or Qt file dialog would otherwise be — Chrome's upload picker,
# its "Save as", every "Attach a file" in every web app.
#
# Install it next to the portal's own wrappers, or anywhere the portal's
# modified PATH reaches (its own config dir first, then
# /usr/share/xdg-desktop-portal-termfilechooser, then $PATH), and point the
# portal at it:
#
#     # ~/.config/xdg-desktop-portal-termfilechooser/config
#     [filechooser]
#     cmd=delightfile-wrapper.sh
#
#     # ~/.config/xdg-desktop-portal/portals.conf
#     [preferred]
#     org.freedesktop.impl.portal.FileChooser=termfilechooser
#
# The shipped wrappers all start a terminal, because the file managers they
# wrap are terminal programs. delightfile is not: it is launched directly and
# the portal waits on it, which is the whole difference between this file and
# `yazi-wrapper.sh`.
#
# See xdg-desktop-portal-termfilechooser(5) for the arguments.

multiple="$1"  # 1 when the dialog will take more than one path
directory="$2" # 1 when it wants a directory rather than a file
save="$3"      # 1 when it is a save dialog
path="$4"      # the suggested file or directory
out="$5"       # where to write what was picked, one path per line
debug="$6"

if [ "$debug" = 1 ]; then
    set -x
fi

# The portal's three answers to "what kind of dialog is this" go through as
# they are. delightfile draws its own Select / Choose folder / Save button in
# the top row, and a button has to know what it is selecting: one file or
# several, a file or a folder, something that exists or a name to save to.
# Every pick — a folder included — is written by the window itself, so `q`,
# `Esc` and closing the window cancel every kind of dialog alike.
set -- --chooser-file="$out"
if [ "$multiple" = 1 ]; then
    set -- "$@" --chooser-multiple
fi
if [ "$directory" = 1 ]; then
    set -- "$@" --chooser-directory
fi
if [ "$save" = 1 ]; then
    # The portal has already created `$path` (with instructions in it, unless
    # create_help_file=0), so the suggested name is sitting under the cursor
    # the moment the window appears, and picking it asks nothing.
    set -- "$@" --chooser-save
fi

# The portal resolves `$path` from open_mode/save_mode and should always send
# one. An empty argument would be read as a directory named "", so it is
# dropped here and delightfile's own default — the directory the process starts
# in — answers instead of a warning in the log. After `--`, so a name that
# happens to begin with a dash is still a name.
if [ -n "$path" ]; then
    set -- "$@" -- "$path"
fi

# `|| true`, and no `set -e` anywhere: a picker that crashed has written
# nothing, and nothing is exactly what a cancel is. Exiting non-zero instead
# would have the portal report a wrapper that could not run, which is a
# different message about the same empty answer.
delightfile "$@" || true

# An empty `$out` is the cancel: the portal reads no lines and tells the
# application the dialog was dismissed. Nothing here writes one on purpose —
# see `write_chooser_file` in crates/df-app/src/cli.rs.
