#!/usr/bin/env bash
# Creates a STABLE self-signed code-signing identity in the login keychain so
# TabTypist.app keeps a constant "designated requirement" across rebuilds.
#
# Why this matters: with ad-hoc signing (`codesign --sign -`), macOS keys the
# Input Monitoring / Accessibility grant on the binary's cdhash, which changes
# on every rebuild — so the grant is silently revoked each time and CGEventTap
# stops receiving events ("Tab does nothing"). Signing with a fixed identity
# makes TCC match on identifier + certificate instead, so you grant Input
# Monitoring ONCE and it survives every subsequent rebuild.
#
# Run this ONCE:  bash scripts/make-signing-cert.sh
# It is idempotent — re-running is a no-op if the identity already exists.
set -euo pipefail

CERT_NAME="TabTypist Dev"

if security find-identity -p codesigning | grep -q "$CERT_NAME"; then
    echo "✅ Signing identity '$CERT_NAME' already exists — nothing to do."
    exit 0
fi

echo "==> Creating self-signed code-signing certificate '$CERT_NAME'..."
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/cert.cnf" <<EOF
[ req ]
distinguished_name = dn
x509_extensions    = ext
prompt             = no
[ dn ]
CN = $CERT_NAME
[ ext ]
basicConstraints   = critical, CA:false
keyUsage           = critical, digitalSignature
extendedKeyUsage   = critical, codeSigning
EOF

openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TMP/key.pem" -out "$TMP/cert.pem" \
    -days 3650 -config "$TMP/cert.cnf" 2>/dev/null

# -legacy + SHA1 MAC: OpenSSL 3.x defaults to AES-256 / SHA-256 PKCS12, which
# Apple's `security import` rejects with "MAC verification failed". The legacy
# 3DES/SHA1 encoding is what macOS can read.
openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -out "$TMP/cert.p12" -passout pass:tabtypist -name "$CERT_NAME" \
    -legacy -macalg sha1 -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES

# Import into the login keychain. -T grants /usr/bin/codesign access to the
# private key so signing won't prompt. macOS may still pop a one-time keychain
# password dialog to authorise the import — that's expected.
security import "$TMP/cert.p12" \
    -k "$HOME/Library/Keychains/login.keychain-db" \
    -P tabtypist -T /usr/bin/codesign

echo
echo "✅ Created signing identity '$CERT_NAME'."
echo "   Now rebuild:  bash scripts/bundle.sh"
echo "   Then grant Input Monitoring to TabTypist.app ONCE — it will persist."
