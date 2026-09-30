#!/usr/bin/env bash
# delightfile.app (plans/other-platforms/06-build-and-release.md B6.16): the
# release binary, the FFmpeg it links and everything that FFmpeg links in
# turn, and pdfium, in one bundle that starts on a Mac with no Homebrew.
#
#   cargo rustc --release --locked -p df-app --bin delightfile -- \
#       -C link-arg=-Wl,-headerpad_max_install_names
#   bash build/macos/fetch-pdfium.sh
#   bash build/macos/bundle.sh [--binary PATH] [--pdfium PATH] [--out DIR]
#
# --binary defaults to target/release/delightfile, --pdfium to what
# fetch-pdfium.sh unpacks, --out to target/dist. The path of the .app is the
# one line on stdout. The header padding in the build line gives
# install_name_tool room to write the bundle's longer library paths into the
# program; without it the rewrite can fail, and this script says so.
#
# What it does, in order:
#
# - Copies the program to Contents/MacOS and pdfium to Contents/Frameworks,
#   then walks `otool -L` from both: every library that is not the system's
#   (/usr/lib, /System) is copied into Contents/Frameworks under the name it
#   is linked by, and walked in turn. Homebrew links by absolute path, and a
#   library that says @rpath or @loader_path is found where the file that
#   names it would have found it.
# - Points every one of those links, and each copied library's own id, at
#   @executable_path/../Frameworks, and drops search paths that lead out of
#   the bundle, so nothing outside it is ever asked for.
# - Fills Info.plist: the version, and LSMinimumSystemVersion as the newest
#   `minos` among the program and its libraries, because dyld refuses to load
#   a library built for a newer macOS than the one running, and Homebrew
#   builds its bottles for the macOS they are poured on.
# - Draws the icon from build/delightfile.svg at 1024 px with sips and makes
#   the .icns with iconutil.
# - Fills Contents/Resources/Licenses: delightfile's LICENSE, FFmpeg's
#   COPYING.GPLv3 and LICENSE.md from the Homebrew keg the bundled FFmpeg
#   came from, pdfium's license files, and SOURCES.txt, which names the exact
#   FFmpeg and where its source is (the Homebrew formula's URL, read with
#   `brew info` and jq, which macOS 15 has), and the Homebrew formula of every
#   other library bundled. An FFmpeg that is not a Homebrew keg stops the
#   script, since then nothing says where its source is.
# - Signs every library and then the bundle ad hoc (`codesign --sign -`):
#   Apple silicon runs no arm64 code without a signature, and a person can
#   still take the unsigned-app route in the README. Not notarized (§6).
# - Runs build/macos/check-app.sh on the result.
#
# Written for the bash 3.2 that macOS ships.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
bin="$repo/target/release/delightfile"
pdfium="$repo/target/pdfium/mac-arm64/lib/libpdfium.dylib"
out="$repo/target/dist"
while [[ $# -gt 0 ]]; do
    case $1 in
    --binary) bin="$2"; shift 2 ;;
    --pdfium) pdfium="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *)
        echo "usage: bash build/macos/bundle.sh [--binary PATH] [--pdfium PATH] [--out DIR]" >&2
        exit 2
        ;;
    esac
done

fail() {
    echo "bundle.sh: $*" >&2
    exit 1
}

version="$(awk -F'"' '/^\[workspace\.package\]/ { s = 1; next } /^\[/ { s = 0 } s && /^version *=/ { print $2; exit }' "$repo/Cargo.toml")"
[[ -x $bin ]] || fail "no release binary at $bin (see the build line at the top of this script)"
said="$("$bin" --version)"
[[ $said == "delightfile $version" ]] || fail "$bin says '$said' but Cargo.toml's version is $version; rebuild it"
[[ -f $pdfium ]] || fail "no libpdfium.dylib at $pdfium; run: bash build/macos/fetch-pdfium.sh"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
app="$out/delightfile.app"
contents="$app/Contents"
frameworks="$contents/Frameworks"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks"
cp "$bin" "$contents/MacOS/delightfile"
chmod 755 "$contents/MacOS/delightfile"

# ── Libraries ───────────────────────────────────────────────────────────────

is_system() {
    case $1 in
    /usr/lib/* | /System/*) return 0 ;;
    *) return 1 ;;
    esac
}

# A Mach-O file's own id, empty for an executable.
own_id() {
    otool -D "$1" | sed -n 2p
}

# The libraries a Mach-O file links, one path per line, without its own id.
links() {
    local id
    id="$(own_id "$1")"
    otool -L "$1" | sed -n '2,$p' | sed -E 's/^[[:space:]]+//; s/ \(compatibility version .*\)$//' |
        while IFS= read -r path; do
            if [[ -n $path && $path != "$id" ]]; then
                echo "$path"
            fi
        done
}

# A Mach-O file's LC_RPATH entries, one per line.
rpaths() {
    otool -l "$1" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }'
}

# The file a link names, as dyld would find it for the file at $2 (the
# library's original place, not its copy in the bundle).
resolve() {
    local link=$1 from=$2 dir rpath candidate
    dir="$(dirname "$from")"
    case $link in
    @loader_path/*) candidate="$dir/${link#@loader_path/}" ;;
    @executable_path/*) candidate="$(dirname "$bin")/${link#@executable_path/}" ;;
    @rpath/*)
        candidate=""
        while IFS= read -r rpath; do
            rpath="${rpath/#@loader_path/$dir}"
            rpath="${rpath/#@executable_path/$(dirname "$bin")}"
            if [[ -f $rpath/${link#@rpath/} ]]; then
                candidate="$rpath/${link#@rpath/}"
                break
            fi
        done < <(
            rpaths "$from"
            rpaths "$bin"
        )
        ;;
    *) candidate="$link" ;;
    esac
    [[ -n $candidate && -f $candidate ]] || fail "$from links $link, which is not there"
    echo "$candidate"
}

# Copy into Frameworks every non-system library the file at $1 (whose
# original is $2) links, and walk each new one.
gather() {
    local file=$1 from=$2 link name source
    while IFS= read -r link; do
        is_system "$link" && continue
        name="$(basename "$link")"
        [[ -e $frameworks/$name ]] && continue
        source="$(resolve "$link" "$from")"
        echo "  $name  ← $source" >&2
        cp -L "$source" "$frameworks/$name"
        chmod 644 "$frameworks/$name"
        echo "$name $source" >>"$work/sources"
        gather "$frameworks/$name" "$source"
    done < <(links "$file")
}

echo "bundling into $app" >&2
cp -L "$pdfium" "$frameworks/libpdfium.dylib"
chmod 644 "$frameworks/libpdfium.dylib"
echo "  libpdfium.dylib  ← $pdfium" >&2
: >"$work/sources"
gather "$contents/MacOS/delightfile" "$bin"
gather "$frameworks/libpdfium.dylib" "$pdfium"

# Every link to a bundled library now goes through @executable_path, every
# bundled library says that is where it lives, and no search path leads out.
relink() {
    local file=$1 link name args=()
    while IFS= read -r link; do
        is_system "$link" && continue
        name="$(basename "$link")"
        [[ $link == "@executable_path/../Frameworks/$name" ]] && continue
        args+=(-change "$link" "@executable_path/../Frameworks/$name")
    done < <(links "$file")
    if [[ $file == "$frameworks"/* ]]; then
        args+=(-id "@executable_path/../Frameworks/$(basename "$file")")
    fi
    while IFS= read -r link; do
        [[ $link == @* ]] || args+=(-delete_rpath "$link")
    done < <(rpaths "$file")
    [[ ${#args[@]} -gt 0 ]] || return 0
    install_name_tool "${args[@]}" "$file" 2> >(grep -v 'invalidate the code signature' >&2) ||
        fail "install_name_tool could not relink $file; a program needs -Wl,-headerpad_max_install_names (see the build line at the top)"
}

relink "$contents/MacOS/delightfile"
for lib in "$frameworks"/*.dylib; do
    relink "$lib"
done

# ── Info.plist ──────────────────────────────────────────────────────────────

minos() {
    otool -l "$1" | awk '
        /cmd LC_BUILD_VERSION/ { build = 1 }
        /cmd LC_VERSION_MIN_MACOSX/ { old = 1 }
        build && $1 == "minos" { print $2; exit }
        old && $1 == "version" { print $2; exit }
    '
}

echo "minos of each file:" >&2
for file in "$contents/MacOS/delightfile" "$frameworks"/*.dylib; do
    echo "$(minos "$file") $(basename "$file")"
done | sort -t. -k1,1n -k2,2n -k3,3n | tee "$work/minos" | sed 's/^/  /' >&2
minimum="$(tail -1 "$work/minos" | cut -d' ' -f1)"
[[ -n $minimum ]] || fail "could not read the minimum macOS of the bundled files"
echo "LSMinimumSystemVersion $minimum (set by $(tail -1 "$work/minos" | cut -d' ' -f2))" >&2

# CFBundleVersion and CFBundleShortVersionString are numbers and dots only,
# so a pre-release suffix stays in the file names and out of the plist.
sed -e "s/@VERSION@/${version%%-*}/g" -e "s/@MINIMUM_SYSTEM_VERSION@/$minimum/g" \
    "$here/Info.plist" >"$contents/Info.plist"
plutil -lint "$contents/Info.plist" >&2

# ── Icon ────────────────────────────────────────────────────────────────────

# A copy of the SVG that says it is 1024 px square, so it is drawn at that
# size rather than drawn at 48 and scaled up.
sed -e 's/width="48" height="48"/width="1024" height="1024"/' "$repo/build/delightfile.svg" >"$work/icon.svg"
cmp -s "$repo/build/delightfile.svg" "$work/icon.svg" &&
    fail "build/delightfile.svg no longer says width=\"48\" height=\"48\"; update the sed above"
if sips -s format png "$work/icon.svg" --out "$work/icon-1024.png" >/dev/null 2>&1; then
    echo "icon: drawn by sips" >&2
else
    # Older macOS has no SVG reader behind sips; Quick Look's thumbnailer
    # draws SVG through WebKit.
    qlmanage -t -s 1024 -o "$work" "$work/icon.svg" >/dev/null 2>&1 || true
    [[ -f $work/icon.svg.png ]] && mv "$work/icon.svg.png" "$work/icon-1024.png"
    echo "icon: sips could not read the SVG; drawn by Quick Look" >&2
fi
[[ -f $work/icon-1024.png ]] || fail "neither sips nor Quick Look could draw build/delightfile.svg"
width="$(sips -g pixelWidth "$work/icon-1024.png" | awk '/pixelWidth/ { print $2 }')"
[[ $width == 1024 ]] || fail "the icon was drawn $width px wide, not 1024"
mkdir "$work/delightfile.iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$work/icon-1024.png" \
        --out "$work/delightfile.iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$work/icon-1024.png" \
        --out "$work/delightfile.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/delightfile.iconset" -o "$contents/Resources/delightfile.icns"

# ── Licenses ────────────────────────────────────────────────────────────────

# The Homebrew keg a bundled library came from, as `<formula> <version>
# <keg>`, or nothing when it came from anywhere else.
keg_of() {
    local dir
    dir="$(cd "$(dirname "$1")" && pwd -P)"
    case $dir in
    */Cellar/*/*/lib) ;;
    *) return 0 ;;
    esac
    dir="$(dirname "$dir")"
    echo "$(basename "$(dirname "$dir")") $(basename "$dir") $dir"
}

licenses="$contents/Resources/Licenses"
mkdir -p "$licenses/delightfile" "$licenses/FFmpeg" "$licenses/pdfium"
cp "$repo/LICENSE" "$licenses/delightfile/LICENSE"

ffmpeg_source="$(awk '$1 ~ /^libavcodec\./ { sub(/^[^ ]+ /, ""); print; exit }' "$work/sources")"
[[ -n $ffmpeg_source ]] || fail "the program links no libavcodec"
read -r ffmpeg_formula ffmpeg_pkg ffmpeg_keg <<<"$(keg_of "$ffmpeg_source")"
[[ ${ffmpeg_formula:-} == ffmpeg* ]] ||
    fail "FFmpeg ($ffmpeg_source) is not a Homebrew keg, so SOURCES.txt cannot say where its source is"
for file in COPYING.GPLv3 LICENSE.md; do
    [[ -f $ffmpeg_keg/$file ]] || fail "FFmpeg's keg $ffmpeg_keg has no $file"
    cp "$ffmpeg_keg/$file" "$licenses/FFmpeg/$file"
done

pdfium_root="$(dirname "$(dirname "$pdfium")")"
[[ -f $pdfium_root/LICENSE ]] || fail "no LICENSE in $pdfium_root; run build/macos/fetch-pdfium.sh"
cp "$pdfium_root/LICENSE" "$licenses/pdfium/LICENSE"
# The licenses of what pdfium builds in (FreeType, ICU, libjpeg-turbo, …).
if [[ -d $pdfium_root/licenses ]]; then
    cp "$pdfium_root"/licenses/* "$licenses/pdfium/"
fi

# What Homebrew says about each formula a bundled library came from, one
# tab-separated line each: name, version, revision, source URL, sha256,
# license.
while IFS= read -r line; do
    keg_of "${line#* }"
done <"$work/sources" | sort -u >"$work/kegs"
# shellcheck disable=SC2046 # one formula name per word
brew info --json=v2 --formula $(cut -d' ' -f1 "$work/kegs" | sort -u) >"$work/brew.json"
# A source that is a git repository (x264's) names its commit.
jq -r '.formulae[] | [.name, .versions.stable, (.revision | tostring),
        (.urls.stable.url + (if .urls.stable.revision then " at " + .urls.stable.revision else "" end)),
        (.urls.stable.checksum // "-"), (.license // "-")] | @tsv' "$work/brew.json" >"$work/formulae"

# A formula's line, and whether its version is the one bundled (`pkg` is
# the keg's folder name, `<version>` or `<version>_<revision>`).
formula_line() {
    awk -F'\t' -v name="$1" '$1 == name' "$work/formulae"
}
same_version() {
    local stable=$2 revision=$3 pkg=$1
    [[ $pkg == "$stable" && $revision == 0 ]] || [[ $pkg == "${stable}_$revision" ]]
}

IFS=$'\t' read -r _ ffmpeg_stable ffmpeg_revision ffmpeg_url ffmpeg_sha ffmpeg_license <<<"$(formula_line "$ffmpeg_formula")"
same_version "$ffmpeg_pkg" "$ffmpeg_stable" "$ffmpeg_revision" ||
    fail "Homebrew's $ffmpeg_formula is $ffmpeg_stable (revision $ffmpeg_revision) but the bundled keg is $ffmpeg_pkg, so its URL is not this FFmpeg's source"
pdfium_release="$(sed -n 1p "$here/pdfium.lock")"
pdfium_release="${pdfium_release#*/download/}"
pdfium_release="${pdfium_release%/*}"

{
    echo "Where the source is of what delightfile.app carries"
    echo
    echo "delightfile $version"
    echo "  License: GPL-3.0-or-later, Licenses/delightfile/LICENSE"
    echo "  Source:  https://github.com/brianschwabauer/delightfile (tag v$version)"
    echo
    echo "FFmpeg $ffmpeg_stable, built by Homebrew (formula $ffmpeg_formula $ffmpeg_pkg), in"
    echo "Contents/Frameworks as libavcodec, libavformat, libavutil, libswresample and"
    echo "libswscale. Built with --enable-gpl."
    echo "  License: $ffmpeg_license (Homebrew's formula), Licenses/FFmpeg/COPYING.GPLv3"
    echo "           and Licenses/FFmpeg/LICENSE.md"
    echo "  Source:  $ffmpeg_url"
    echo "  sha256:  $ffmpeg_sha"
    echo "  Recipe:  https://formulae.brew.sh/formula/$ffmpeg_formula"
    echo
    echo "The libraries FFmpeg links, built by Homebrew, in Contents/Frameworks"
    echo "(each formula's page has its recipe and its license):"
    while read -r formula pkg _; do
        [[ $formula == "$ffmpeg_formula" ]] && continue
        IFS=$'\t' read -r _ stable revision url _ license <<<"$(formula_line "$formula")"
        echo "  $formula $pkg ($license)"
        if same_version "$pkg" "$stable" "$revision"; then
            echo "    Source: $url"
        else
            echo "    Source: the $pkg release of $formula (Homebrew's formula has moved to $stable)"
        fi
        echo "    Recipe: https://formulae.brew.sh/formula/$formula"
    done <"$work/kegs"
    echo
    echo "pdfium, bblanchon/pdfium-binaries $pdfium_release, in Contents/Frameworks as"
    echo "libpdfium.dylib"
    echo "  License: Licenses/pdfium/LICENSE, and beside it the licenses of what it builds in"
    echo "  Source:  https://github.com/bblanchon/pdfium-binaries/releases/tag/$pdfium_release"
} >"$licenses/SOURCES.txt"
echo "Licenses/SOURCES.txt:" >&2
sed 's/^/  /' "$licenses/SOURCES.txt" >&2

# ── Signature ───────────────────────────────────────────────────────────────

xattr -cr "$app"
for lib in "$frameworks"/*.dylib; do
    codesign --force --sign - --timestamp=none "$lib"
done
codesign --force --sign - --timestamp=none "$app"

bash "$here/check-app.sh" "$app" >&2
echo "$app"
