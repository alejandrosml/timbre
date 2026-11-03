#!/usr/bin/env bash
# Cross-compile the universal macOS binary from Linux with cargo-zigbuild.
# Release builds are made on macOS in CI; this is for development only.
# The only Apple framework Timbre links is CoreFoundation (through chrono's
# time-zone lookup); a text stub lists the few symbols needed.
set -euo pipefail
cd "$(dirname "$0")/.."
STUBS="$PWD/build/macos-stubs"
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS="-C link-arg=-F$STUBS"
export CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS="-C link-arg=-F$STUBS"
export MACOSX_DEPLOYMENT_TARGET=12.0
cargo zigbuild --release --locked --target universal2-apple-darwin "$@"
echo "built target/universal2-apple-darwin/release/timbre"
