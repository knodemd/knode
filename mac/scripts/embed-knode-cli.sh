#!/bin/sh
# Copies the knode command into the app, at Knode.app/Contents/Helpers/knode, and signs it with
# the identity of the app (ad hoc for local builds) and the hardened runtime, before Xcode signs
# the app around it.
#
# Usage: embed-knode-cli.sh <built knode> <path inside the app>
set -eu

source="$1"
destination="$2"
identity="${EXPANDED_CODE_SIGN_IDENTITY:-}"
[ -n "$identity" ] || identity="-"

mkdir -p "$(dirname "$destination")"
ditto "$source" "$destination"
if [ "$identity" = "-" ]; then
    codesign --force --sign - --options runtime --identifier md.knode.knode --timestamp=none "$destination"
else
    codesign --force --sign "$identity" --options runtime --identifier md.knode.knode --timestamp "$destination"
fi
