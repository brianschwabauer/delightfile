#!/usr/bin/env bash
# The checks a delightfile.app has to pass before it ships
# (plans/other-platforms/06-build-and-release.md B6.16, and B6.17 on the copy
# inside the dmg):
#
#   bash build/macos/check-app.sh path/to/delightfile.app
#
# - Info.plist lints and names the executable, the icon is there.
# - `codesign --verify --deep --strict` accepts the ad hoc signature.
# - `otool -L` on the program and every bundled library names only the
#   system's libraries and the bundle's own Contents/Frameworks, and every
#   bundled library a link names is there.
# - The program runs (`--version`), and the libraries dyld actually loads for
#   it are the system's and the bundle's and nothing else, however much
#   Homebrew the machine has.
#
# Written for the bash 3.2 that macOS ships.
set -euo pipefail

app="$(cd "$1" && pwd -P)"
contents="$app/Contents"
frameworks="$contents/Frameworks"
program="$contents/MacOS/delightfile"
problems=0

problem() {
    echo "check-app: $*" >&2
    problems=$((problems + 1))
}

plutil -lint "$contents/Info.plist"
plist() {
    /usr/libexec/PlistBuddy -c "Print :$1" "$contents/Info.plist"
}
[[ $(plist CFBundleExecutable) == delightfile ]] || problem "CFBundleExecutable is not delightfile"
[[ -f $contents/Resources/$(plist CFBundleIconFile) ]] || problem "no Resources/$(plist CFBundleIconFile)"
[[ -f $frameworks/libpdfium.dylib ]] || problem "no Frameworks/libpdfium.dylib"
echo "$(plist CFBundleIdentifier) $(plist CFBundleShortVersionString), LSMinimumSystemVersion $(plist LSMinimumSystemVersion)"

codesign --verify --deep --strict --verbose=2 "$app"

for file in "$program" "$frameworks"/*.dylib; do
    echo "otool -L ${file#"$app/"}:"
    id="$(otool -D "$file" | sed -n 2p)"
    while IFS= read -r link; do
        echo "  $link"
        [[ $link == "$id" ]] && continue
        case $link in
        /usr/lib/* | /System/*) ;;
        @executable_path/../Frameworks/*)
            [[ -f $frameworks/${link#@executable_path/../Frameworks/} ]] ||
                problem "${file#"$app/"} links $link, which the bundle does not hold"
            ;;
        *) problem "${file#"$app/"} links $link, outside the system and the bundle" ;;
        esac
    done < <(otool -L "$file" | sed -n '2,$p' | sed -E 's/^[[:space:]]+//; s/ \(compatibility version .*\)$//')
done

# DYLD_PRINT_LIBRARIES names every image dyld maps for the program, the
# shared cache's included; the ad hoc signature has no hardened runtime, so
# dyld honours it.
said="$(DYLD_PRINT_LIBRARIES=1 "$program" --version 2>"${TMPDIR:-/tmp}/check-app-dyld.$$")"
echo "--version: $said"
[[ $said == "delightfile $(plist CFBundleShortVersionString)"* ]] ||
    problem "--version says '$said'"
loaded=0
while IFS= read -r line; do
    # `dyld[<pid>]: <UUID> <path>` (older dyld prints no UUID), among notes
    # of dyld's own such as "move loaded to delayed: …", which name no path.
    path="${line#dyld\[*\]: }"
    [[ $path == "$line" ]] && continue
    path="${path#<*> }"
    [[ $path == /* ]] || continue
    loaded=$((loaded + 1))
    case $path in
    /usr/lib/* | /System/* | "$app"/*) ;;
    *) problem "dyld loaded $path from outside the system and the bundle" ;;
    esac
done <"${TMPDIR:-/tmp}/check-app-dyld.$$"
echo "dyld loaded $loaded images for --version; the bundle's:"
grep -F "$app/" "${TMPDIR:-/tmp}/check-app-dyld.$$" | sed -E 's/^dyld\[[0-9]+\]: (<[^>]*> )?/  /' || true
rm -f "${TMPDIR:-/tmp}/check-app-dyld.$$"
[[ $loaded -gt 0 ]] || problem "DYLD_PRINT_LIBRARIES printed nothing, so what was loaded is unknown"

if [[ $problems -gt 0 ]]; then
    echo "check-app: $problems problem(s) with $app" >&2
    exit 1
fi
echo "check-app: $app passes"
