#!/bin/sh
# Builds the Knode core for the Mac app: KnodeCore.xcframework (the Rust static library for Apple
# silicon and Intel) and KnodeCore.swift (its Swift bindings, generated with UniFFI).
#
# Usage: build-xcframework.sh [output folder]   (default: mac/build/KnodeCore)
#
# Xcode runs it before building the KnodeCore target; it also runs on its own. Everything it
# writes is generated, so the output folder is never committed.
set -eu

mac_dir="$(cd "$(dirname "$0")/.." && pwd -P)"
core_dir="$(cd "$mac_dir/../core" && pwd -P)"
out_dir="${1:-$mac_dir/build/KnodeCore}"

# Xcode runs scripts without the login shell's PATH.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
    exit 1
fi

# Xcode only reruns this script when a file in the input list changes, so the list must name
# every source file of the crates of this workspace it is built from.
"$mac_dir/scripts/update-inputs.sh" --check core

# A build folder of its own, so the settings Xcode exports never invalidate core/target.
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

cd "$core_dir"
libraries=""
for target in $targets; do
    cargo build --locked --release --package knode-ffi --lib --target "$target"
    libraries="$libraries $CARGO_TARGET_DIR/$target/release/libknode_ffi.a"
done

work="$CARGO_TARGET_DIR/xcframework"
rm -rf "$work"
mkdir -p "$work/lib" "$work/include/KnodeCoreFFI" "$work/swift"

# One universal library for both Mac architectures.
# shellcheck disable=SC2086
lipo -create $libraries -output "$work/lib/libknode_ffi.a"

cargo run --locked --quiet --package knode-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/libknode_ffi.a" \
    --language swift --out-dir "$work/swift"

# In a folder of its own, so its module map never meets another framework's.
cp "$work/swift/KnodeCoreFFI.h" "$work/include/KnodeCoreFFI/"
cp "$work/swift/KnodeCoreFFI.modulemap" "$work/include/KnodeCoreFFI/module.modulemap"

rm -rf "$out_dir/KnodeCore.xcframework"
mkdir -p "$out_dir"
xcodebuild -create-xcframework \
    -library "$work/lib/libknode_ffi.a" -headers "$work/include" \
    -output "$out_dir/KnodeCore.xcframework" >/dev/null
cp "$work/swift/KnodeCore.swift" "$out_dir/KnodeCore.swift"

echo "Built $out_dir/KnodeCore.xcframework and KnodeCore.swift"
