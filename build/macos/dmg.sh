#!/usr/bin/env bash
# The macOS release download (plans/other-platforms/06-build-and-release.md
# B6.17): delightfile-<version>-aarch64-macos.dmg, a compressed disk image
# holding delightfile.app and a link to /Applications to drag it onto.
#
#   bash build/macos/bundle.sh
#   bash build/macos/dmg.sh [--app PATH] [--out DIR] [--check]
#
# --app defaults to target/dist/delightfile.app and --out to target/dist.
# --check runs check-dmg.sh on the result: mounted read-only, the app inside
# checked where it stands, unmounted. The path of the dmg is the one line on
# stdout.
#
# HFS+ rather than APFS, so that the image mounts on every macOS the app can
# start on, and 7-Zip can list it elsewhere. hdiutil on CI runners now and
# then answers "Resource busy"; `create` gets three tries.
#
# Written for the bash 3.2 that macOS ships.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
app="$repo/target/dist/delightfile.app"
out="$repo/target/dist"
check=0
while [[ $# -gt 0 ]]; do
    case $1 in
    --app) app="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --check) check=1; shift ;;
    *)
        echo "usage: bash build/macos/dmg.sh [--app PATH] [--out DIR] [--check]" >&2
        exit 2
        ;;
    esac
done

if [[ ! -d $app/Contents/MacOS ]]; then
    echo "dmg.sh: no app at $app; run build/macos/bundle.sh" >&2
    exit 1
fi
version="$(awk -F'"' '/^\[workspace\.package\]/ { s = 1; next } /^\[/ { s = 0 } s && /^version *=/ { print $2; exit }' "$repo/Cargo.toml")"
mkdir -p "$out"
dmg="$(cd "$out" && pwd)/delightfile-$version-aarch64-macos.dmg"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/delightfile"
cp -R "$app" "$stage/delightfile/"
ln -s /Applications "$stage/delightfile/Applications"

rm -f "$dmg"
made=0
for try in 1 2 3; do
    if hdiutil create -volname delightfile -srcfolder "$stage/delightfile" \
        -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$dmg" >&2; then
        made=1
        break
    fi
    echo "hdiutil create failed (try $try of 3)" >&2
    sleep 5
done
if [[ $made -eq 0 ]]; then
    echo "dmg.sh: hdiutil could not make $dmg" >&2
    exit 1
fi
echo "made $(basename "$dmg") ($(du -h "$dmg" | cut -f1))" >&2

if [[ $check -eq 1 ]]; then
    bash "$here/check-dmg.sh" "$dmg" >&2
fi
echo "$dmg"
