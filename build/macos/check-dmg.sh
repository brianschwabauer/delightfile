#!/usr/bin/env bash
# B6.17's acceptance check for the macOS download: the dmg verifies, mounts
# read-only, holds delightfile.app and a link to /Applications, and the app
# inside passes check-app.sh as it stands on the image. Then it is unmounted.
#
#   bash build/macos/check-dmg.sh delightfile-<version>-aarch64-macos.dmg
#
# hdiutil on CI runners now and then answers "Resource busy"; attach and
# detach get three tries each.
#
# Written for the bash 3.2 that macOS ships.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dmg="$1"

hdiutil_retrying() {
    local try
    for try in 1 2 3; do
        if hdiutil "$@"; then
            return 0
        fi
        echo "hdiutil $1 failed (try $try of 3)" >&2
        sleep 5
    done
    return 1
}

mount="$(mktemp -d)"
mounted=0
cleanup() {
    [[ $mounted -eq 0 ]] || hdiutil detach "$mount" -force >/dev/null 2>&1 || true
    rmdir "$mount" 2>/dev/null || true
}
trap cleanup EXIT

hdiutil verify "$dmg"
hdiutil_retrying attach "$dmg" -readonly -nobrowse -noautoopen -mountpoint "$mount"
mounted=1
echo "mounted $(basename "$dmg") at $mount:"
ls -l "$mount"
if [[ ! -L $mount/Applications || $(readlink "$mount/Applications") != /Applications ]]; then
    echo "check-dmg: the image has no Applications link" >&2
    exit 1
fi
bash "$here/check-app.sh" "$mount/delightfile.app"
hdiutil_retrying detach "$mount"
mounted=0
echo "check-dmg: $(basename "$dmg") mounts, holds the Applications link, its app passes check-app.sh, and it unmounts"
