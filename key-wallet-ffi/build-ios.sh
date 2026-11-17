#!/bin/bash

# Build script for key-wallet-ffi iOS targets

set -e

echo "Building key-wallet-ffi for iOS..."

# Ensure we have the required iOS targets
rustup target add aarch64-apple-ios aarch64-apple-ios-sim

# Build for iOS devices (arm64)
echo "Building for iOS devices (arm64)..."
cargo build --profile mobile --target aarch64-apple-ios

# Build for iOS simulator (arm64 - Apple Silicon Macs)
echo "Building for iOS simulator (arm64)..."
cargo build --profile mobile --target aarch64-apple-ios-sim

# Create output directory
echo "Creating output directory..."
mkdir -p target/universal/mobile

# Copy simulator library (no need for lipo since we only have one architecture)
cp target/aarch64-apple-ios-sim/mobile/libkey_wallet_ffi.a target/universal/mobile/libkey_wallet_ffi_sim.a

# Copy device library
cp target/aarch64-apple-ios/mobile/libkey_wallet_ffi.a target/universal/mobile/libkey_wallet_ffi_device.a

echo "Build complete!"
echo "Libraries available at:"
echo "  - Device: target/universal/mobile/libkey_wallet_ffi_device.a"
echo "  - Simulator: target/universal/mobile/libkey_wallet_ffi_sim.a"
