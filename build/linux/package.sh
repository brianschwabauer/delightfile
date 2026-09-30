#!/usr/bin/env bash
# The Linux release tarball (plans/other-platforms/06-build-and-release.md
# B6.14): delightfile-<version>-x86_64-linux.tar.gz, one folder holding the
# release binary, build/install.sh and every file install.sh installs, laid
# out as build/ is, so that in the unpacked folder `bash install.sh` installs
# the tarball's binary exactly as the repository's install does.
#
#   cargo build --release --locked -p df-app
#   bash build/linux/package.sh [--binary PATH] [--out DIR]
#
# --binary defaults to target/release/delightfile and --out to target/dist.
# The path of the tarball is the one line on stdout.
#
# The binary links the system's FFmpeg 9 (libavcodec 63) as a build from
# source does, so the tarball is for a machine that has it: on Arch,
# `pacman -S ffmpeg`. install.sh stays the way to install from a checkout;
# this is the same install for someone who did not build.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
bin="$repo/target/release/delightfile"
out="$repo/target/dist"
while [[ $# -gt 0 ]]; do
    case $1 in
    --binary) bin="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *)
        echo "usage: bash build/linux/package.sh [--binary PATH] [--out DIR]" >&2
        exit 2
        ;;
    esac
done

version="$(awk -F'"' '/^\[workspace\.package\]/ { s = 1; next } /^\[/ { s = 0 } s && /^version *=/ { print $2; exit }' "$repo/Cargo.toml")"
if [[ ! -x $bin ]]; then
    echo "no release binary at $bin — run: cargo build --release --locked -p df-app" >&2
    exit 1
fi
said="$("$bin" --version)"
if [[ $said != "delightfile $version" ]]; then
    echo "$bin says '$said' but Cargo.toml's version is $version; rebuild it" >&2
    exit 1
fi

name="delightfile-$version-x86_64-linux"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
dir="$stage/$name"

install -Dm755 "$bin" "$dir/delightfile"
install -Dm755 "$repo/build/install.sh" "$dir/install.sh"
install -Dm755 "$repo/build/delightfile-wrapper.sh" "$dir/delightfile-wrapper.sh"
for file in delightfile.desktop delightfile.svg delightfile.portal \
    org.freedesktop.FileManager1.service.in \
    org.freedesktop.impl.portal.desktop.delightfile.service.in \
    icons/delightfile-48.png icons/delightfile-128.png icons/delightfile-256.png; do
    install -Dm644 "$repo/build/$file" "$dir/$file"
done
install -Dm644 "$repo/LICENSE" "$dir/LICENSE"
install -Dm644 "$repo/README.md" "$dir/README.md"

mkdir -p "$out"
tarball="$(cd "$out" && pwd)/$name.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$stage" -czf "$tarball" "$name"
echo "packaged $name.tar.gz ($(du -h "$tarball" | cut -f1)):" >&2
tar -tzvf "$tarball" >&2
echo "$tarball"
