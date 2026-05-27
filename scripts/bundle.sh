#!/usr/bin/env bash
# Assembles TabTypist.app from compiled Swift and Rust binaries.
# Usage: bash scripts/bundle.sh [--release]
set -euo pipefail

RELEASE_FLAG=""
SWIFT_BUILD_DIR=".build/debug"
RUST_BUILD_DIR="target/debug"

if [ "${1:-}" = "--release" ]; then
    RELEASE_FLAG="--release"
    SWIFT_BUILD_DIR=".build/release"
    RUST_BUILD_DIR="target/release"
fi

OUT_DIR="dist"
APP_DIR="${OUT_DIR}/TabTypist.app"

echo "==> Building Swift..."
if [ -n "$RELEASE_FLAG" ]; then
    swift build -c release
else
    swift build
fi

echo "==> Building Rust..."
cargo build $RELEASE_FLAG -p tabtypist-core

echo "==> Assembling ${APP_DIR}..."
rm -rf "$APP_DIR"
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"

cp "${SWIFT_BUILD_DIR}/TabTypist" "${APP_DIR}/Contents/MacOS/TabTypist"
cp "${RUST_BUILD_DIR}/tabtypist-core" "${APP_DIR}/Contents/Resources/tabtypist-core"
cp "Resources/ed25519_pubkey.bin" "${APP_DIR}/Contents/Resources/ed25519_pubkey.bin"
cp "Resources/Info.plist" "${APP_DIR}/Contents/Info.plist"

echo "==> Done: ${APP_DIR}"
ls -lh "${APP_DIR}/Contents/MacOS/" "${APP_DIR}/Contents/Resources/"
