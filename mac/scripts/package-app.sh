#!/bin/sh
# Packages an already built, signed and stapled universal app. Never installs or launches it.
# Usage: package-app.sh <Knode.app> <version> <output directory> [--allow-adhoc]
# --allow-adhoc is only for local packaging checks, never public releases.
set -eu

fail() { echo "error: $*" >&2; exit 1; }
[ "$#" -ge 3 ] && [ "$#" -le 4 ] || fail "usage: $0 <Knode.app> <version> <output directory> [--allow-adhoc]"
app="$1"
version="$2"
output="$3"
mode="${4:-release}"
case "$mode" in release|--allow-adhoc) ;; *) fail "unknown option: $mode" ;; esac
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || fail "invalid version: $version"
[ -d "$app" ] || fail "app not found: $app"
[ "$(basename "$app")" = Knode.app ] || fail "expected Knode.app"
helper="$app/Contents/Helpers/knode"
[ -x "$helper" ] || fail "the app has no executable embedded knode command"
actual="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
[ "$actual" = "$version" ] || fail "app version $actual does not match $version"
[ "$("$helper" --version)" = "knode $version" ] || fail "embedded knode version does not match $version"
for binary in "$app/Contents/MacOS/Knode" "$helper"; do
    architectures="$(lipo -archs "$binary")"
    for architecture in arm64 x86_64; do
        echo "$architectures" | grep -qw "$architecture" || fail "$binary lacks $architecture"
    done
done
codesign --verify --deep --strict "$app"
if [ "$mode" = release ]; then
    codesign -dv --verbose=4 "$app" 2>&1 | grep -q '^Authority=Developer ID Application:' \
        || fail "a release app must be signed with Developer ID Application"
    xcrun stapler validate "$app"
fi

mkdir -p "$output"
output="$(cd "$output" && pwd -P)"
image="$output/Knode-$version.dmg"
[ ! -e "$image" ] || fail "refusing to overwrite $image"
stage="$(mktemp -d "${TMPDIR:-/tmp}/knode-dmg.XXXXXX")"
trap 'rm -rf "$stage"' EXIT HUP INT TERM
ditto "$app" "$stage/Knode.app"
ln -s /Applications "$stage/Applications"
repo="$(cd "$(dirname "$0")/../.." && pwd -P)"
cp "$repo/LICENSE" "$repo/NOTICE" "$stage/"
hdiutil create -quiet -volname Knode -srcfolder "$stage" -format UDZO "$image"
hdiutil verify -quiet "$image"
(cd "$output" && shasum -a 256 "Knode-$version.dmg" > "Knode-$version.dmg.sha256")
echo "Packaged $image"
