#!/bin/bash

# Build script for dash-spv-ffi iOS targets: an xcframework under dash-spv-ffi/build/

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-ios.sh: the iOS build needs macOS with Xcode; this is $(uname -s)" >&2
    exit 1
fi

for tool in rustup cargo xcodebuild; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "build-ios.sh: $tool not found" >&2
        exit 1
    fi
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$script_dir/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-$root/target}
out=$script_dir/build
framework=$out/DashSpvFFI.xcframework
lib=libdash_spv_ffi.a
targets=(aarch64-apple-ios aarch64-apple-ios-sim)

echo "Building dash-spv-ffi for iOS..."

rustup target add "${targets[@]}"

cd "$root"
for target in "${targets[@]}"; do
    echo "Building for $target..."
    cargo build -p dash-spv-ffi --profile mobile --target "$target"
done

echo "Creating the xcframework..."
rm -rf "$framework" "$out/headers"
args=()
for target in "${targets[@]}"; do
    built=$target_dir/$target/mobile
    if [[ ! -f "$built/$lib" || ! -f "$built/include/dash-spv-ffi/dash-spv-ffi.h" ]]; then
        echo "build-ios.sh: $built has no $lib or no dash-spv-ffi header" >&2
        exit 1
    fi
    mkdir -p "$out/headers/$target"
    cp -R "$built/include/." "$out/headers/$target/"
    args+=(-library "$built/$lib" -headers "$out/headers/$target")
done
xcodebuild -create-xcframework "${args[@]}" -output "$framework"

echo "Build complete!"
echo "XCFramework: $framework"
for target in "${targets[@]}"; do
    echo "  - $target: $target_dir/$target/mobile/$lib"
done
