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

# Codesign. Prefer a STABLE self-signed identity ("TabTypist Dev", created by
# scripts/make-signing-cert.sh): with a real identity, macOS keys Input
# Monitoring / Accessibility grants on the designated requirement (identifier +
# certificate), so the grant survives every rebuild — you grant once.
#
# Ad-hoc (`--sign -`) is the fallback. It keys the grant on the cdhash, which
# changes on every rebuild, so the grant is revoked each time and CGEventTap
# silently drops events ("Tab does nothing"). The --identifier pin only keeps
# the bundle id stable; it does NOT stop the cdhash churn.
SIGN_IDENTITY="${CODESIGN_IDENTITY:-TabTypist Dev}"
# NOTE: no -v — a self-signed identity is untrusted (CSSMERR_TP_NOT_TRUSTED) and
# `-v` would hide it, but it still signs fine and TCC matches on it correctly.
if security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_IDENTITY"; then
    echo "==> Codesigning with stable identity: $SIGN_IDENTITY"
    codesign --force --deep --sign "$SIGN_IDENTITY" \
        --identifier com.tabtypist.TabTypist "${APP_DIR}"
else
    echo "==> ⚠️  No '$SIGN_IDENTITY' identity found — falling back to AD-HOC signing."
    echo "    Input Monitoring will be revoked on every rebuild. To fix permanently:"
    echo "        bash scripts/make-signing-cert.sh"
    codesign --force --sign - --identifier com.tabtypist.TabTypist "${APP_DIR}"
fi

echo "==> Done: ${APP_DIR}"
ls -lh "${APP_DIR}/Contents/MacOS/" "${APP_DIR}/Contents/Resources/"
