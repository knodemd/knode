#!/bin/sh
# Builds the knode command (core/crates/knode-cli) that the Mac app carries inside, as one binary
# for Apple silicon and Intel.
#
# Usage: build-knode-cli.sh [output file]   (default: mac/build/KnodeCLI/knode)
#
# Xcode runs it in the KnodeCLI target; it also runs on its own. The Knode target then copies the
# binary to Knode.app/Contents/Helpers/knode and signs it. Everything it writes is generated, so
# the output is never committed.
set -eu

mac_dir="$(cd "$(dirname "$0")/.." && pwd -P)"
core_dir="$(cd "$mac_dir/../core" && pwd -P)"
output="${1:-$mac_dir/build/KnodeCLI/knode}"

# Xcode runs scripts without the login shell's PATH.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
    exit 1
fi

cd "$core_dir"

# Xcode only reruns this script when a file in the input list changes, so the list must name
# every source file of the crates of this workspace it is built from.
"$mac_dir/scripts/update-inputs.sh" --check cli

# The same build folder as the core of the app, apart from core/target.
export CARGO_TARGET_DIR="$mac_dir/build/cargo"
export MACOSX_DEPLOYMENT_TARGET=26.0
targets="aarch64-apple-darwin x86_64-apple-darwin"

if command -v rustup >/dev/null 2>&1; then
    installed="$(rustup target list --installed)"
    for target in $targets; do
        if ! echo "$installed" | grep -qx "$target"; then
            echo "Adding the Rust target $target"
            rustup target add "$target"
        fi
    done
fi

binaries=""
for target in $targets; do
    cargo build --locked --release --package knode-cli --bin knode --target "$target"
    binaries="$binaries $CARGO_TARGET_DIR/$target/release/knode"
done

mkdir -p "$(dirname "$output")"
# shellcheck disable=SC2086
lipo -create $binaries -output "$output.tmp"
mv -f "$output.tmp" "$output"

echo "Built $output"
