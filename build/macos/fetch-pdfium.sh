#!/usr/bin/env bash
# pdfium for the macOS bundle: the build that build/macos/pdfium.lock pins,
# downloaded, checked against its sha256 and unpacked
# (plans/other-platforms/06-build-and-release.md B6.8).
#
#   bash build/macos/fetch-pdfium.sh [dest]   # dest: target/pdfium/mac-arm64
#
# pdfium.lock is two lines, the tarball's URL and its sha256. The release is
# bblanchon/pdfium-binaries' chromium/7881, the ABI df-app's `pdfium_7881`
# feature binds to; nothing links it at build time, the app opens it at run
# time from the bundle's Contents/Frameworks. The tarball is kept in dest, so
# a second run (or a CI cache of that folder) downloads nothing, and its hash
# is checked on every run, cached or not.
#
# Progress goes to stderr; the one line on stdout is the path of
# libpdfium.dylib, for a caller to take.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
dest="${1:-$repo/target/pdfium/mac-arm64}"

url="$(sed -n 1p "$here/pdfium.lock")"
sha256="$(sed -n 2p "$here/pdfium.lock")"
tgz="$dest/${url##*/}"

mkdir -p "$dest"
if [[ ! -f $tgz ]]; then
    echo "downloading $url" >&2
    curl -fsSL --retry 3 -o "$tgz.part" "$url"
    mv "$tgz.part" "$tgz"
fi
actual="$(shasum -a 256 "$tgz" | cut -d' ' -f1)"
if [[ $actual != "$sha256" ]]; then
    rm -f "$tgz"
    echo "${tgz##*/} has sha256 $actual; pdfium.lock says $sha256" >&2
    exit 1
fi
echo "pdfium: ${url#*/download/}, sha256 $sha256, verified" >&2

rm -rf "${dest:?}/lib" "${dest:?}/licenses"
tar -xzf "$tgz" -C "$dest" lib/libpdfium.dylib LICENSE licenses
echo "$dest/lib/libpdfium.dylib"
