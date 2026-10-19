#!/bin/sh
# Builds KnodeDocument.icns, the icon of .knode packages, from the PNGs in the brand kit.
# Usage: document-icon.sh <brand Document folder> <output .icns>
set -eu

source_dir="$1"
output="$2"
iconset="${TMPDIR:-/tmp}/KnodeDocument-$$.iconset"
trap 'rm -rf "$iconset"' EXIT
mkdir -p "$iconset"

for size in 16 32 128 256 512; do
    double=$((size * 2))
    sips -s format png -z "$size" "$size" "$source_dir/knode-document-icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -s format png -z "$double" "$double" "$source_dir/knode-document-icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$(dirname "$output")"
iconutil -c icns "$iconset" -o "$output"
