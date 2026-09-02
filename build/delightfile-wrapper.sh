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

multiple="$1"  # 1 when the dialog will take more than one file
directory="$2" # 1 when it wants a directory rather than a file
save="$3"      # 1 when it is a save dialog
path="$4"      # the suggested file or directory
out="$5"       # where to write what was picked, one path per line
debug="$6"

if [ "$debug" = 1 ]; then
    set -x
fi

# Neither `multiple` nor `save` changes the command line, and both are named
# above so the argument order reads as itself rather than as a run of "$4"s.
# `multiple` needs nothing because `Enter` already writes the whole selection
# and one file is a selection of one; `save` needs nothing because saving is
# the same gesture as opening, aimed at a name the portal has already created.
: "$multiple" "$save"

# The portal resolves `$path` from open_mode/save_mode and should always send
# one. An empty argument would be read as a directory named "", so it is
# dropped here and delightfile's own default — the directory the process starts
# in — answers instead of a warning in the log.
if [ -n "$path" ]; then
    set -- "$path"
else
    set --
fi

if [ "$directory" = 1 ]; then
    # There is no "pick this folder" key, and there does not need to be: the
    # way you choose a directory in a file manager is to be *in* it. So the
    # session also gets a cwd-file, `q` writes the directory it ended in, and
    # the fallback at the bottom promotes that to the answer when nothing was
    # picked outright. (`Enter` still works — on a file, for the caller that
    # asked for a directory but would accept the one a file is in.)
    delightfile --chooser-file="$out" --cwd-file="$out.1" "$@" || true
else
    # Open and save both land here. In a save the portal has already created
    # `$path` (with instructions in it, unless create_help_file=0), so the file
    # to pick is sitting under the cursor the moment the window appears.
    delightfile --chooser-file="$out" "$@" || true
fi

# `|| true` above, and no `set -e` anywhere: a picker that crashed still has
# to reach this, or it leaves `$out.1` behind and the dialog it was standing in
# for hangs on a temp file nobody will ever delete. A crash is a cancel.
if [ "$directory" = 1 ]; then
    if [ ! -s "$out" ] && [ -s "$out.1" ]; then
        cat "$out.1" >"$out"
    fi
    rm -f "$out.1"
fi

# An empty `$out` is the cancel: the portal reads no lines and tells the
# application the dialog was dismissed. Nothing here writes one on purpose —
# see `write_chooser_file` in crates/df-app/src/cli.rs.
