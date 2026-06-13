#!/usr/bin/env bash
# Builds a styled distributable DMG: TabTypist.app beside an Applications
# symlink, with a branded background, custom window layout, and volume icon.
# Usage: bash scripts/make-dmg.sh
set -euo pipefail

APP="dist/TabTypist.app"
DMG_OUT="dist/TabTypist.dmg"
DMG_RW="dist/TabTypist-rw.dmg"
VOLUME_NAME="TabTypist"
BACKGROUND="Resources/dmg-background.png"
VOLICON="Resources/AppIcon.icns"

[ -d "$APP" ] || { echo "ERROR: $APP not found — run bundle.sh first." >&2; exit 1; }
[ -f "$BACKGROUND" ] || { echo "ERROR: $BACKGROUND not found." >&2; exit 1; }

# Detach any stale TabTypist volumes left mounted by a prior run, otherwise a
# read-only copy can shadow /Volumes/TabTypist and the new mount lands at
# "TabTypist 1", sending our files to the wrong (read-only) volume.
detach_stale() {
    while read -r dev; do
        [ -n "$dev" ] && hdiutil detach "$dev" -force >/dev/null 2>&1 || true
    done < <(mount | awk '/\/Volumes\/TabTypist/ {print $1}')
}
detach_stale

echo "==> Creating blank writable DMG..."
rm -f "$DMG_OUT" "$DMG_RW"
APP_SIZE_MB=$(du -sm "$APP" | awk '{print $1}')
DMG_SIZE_MB=$(( APP_SIZE_MB + 30 ))

# Blank images default to read-write (UDRW); passing -format here is rejected.
hdiutil create \
    -size "${DMG_SIZE_MB}m" \
    -volname "$VOLUME_NAME" \
    -fs HFS+ \
    -ov \
    "$DMG_RW" >/dev/null

ATTACH=$(hdiutil attach -readwrite -noverify -noautoopen "$DMG_RW")
DEVICE=$(echo "$ATTACH" | egrep '^/dev/' | head -1 | awk '{print $1}')
MOUNT=$(echo "$ATTACH" | egrep -o '/Volumes/.*$' | head -1)

cleanup() {
    [ -n "${DEVICE:-}" ] && hdiutil detach "$DEVICE" -force >/dev/null 2>&1 || true
    rm -f "$DMG_RW"
}
trap cleanup EXIT

[ -d "$MOUNT" ] || { echo "ERROR: volume did not mount." >&2; exit 1; }
echo "==> Mounted at $MOUNT"

echo "==> Populating volume..."
cp -R "$APP" "$MOUNT/"
ln -s /Applications "$MOUNT/Applications"
mkdir "$MOUNT/.background"
cp "$BACKGROUND" "$MOUNT/.background/dmg-background.png"
cp "$VOLICON" "$MOUNT/.VolumeIcon.icns"

echo "==> Styling window (best effort)..."
# Finder AppleScript styling can fail on headless CI; never let it abort the build.
osascript <<'APPLESCRIPT' || echo "    (styling skipped — Finder unavailable)"
tell application "Finder"
    tell disk "TabTypist"
        open
        delay 1
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set the bounds of container window to {200, 120, 840, 520}
        set opts to the icon view options of container window
        set arrangement of opts to not arranged
        set icon size of opts to 128
        set text size of opts to 12
        set background picture of opts to file ".background:dmg-background.png"
        set position of item "TabTypist.app" of container window to {165, 200}
        set position of item "Applications" of container window to {475, 200}
        update without registering applications
        delay 1
        close
    end tell
end tell
APPLESCRIPT

# Flag the volume to use its custom .VolumeIcon.icns.
SetFile -a C "$MOUNT" 2>/dev/null || true

sync
hdiutil detach "$DEVICE" >/dev/null 2>&1 || hdiutil detach "$DEVICE" -force >/dev/null 2>&1
DEVICE=""  # detached; let cleanup skip it

echo "==> Compressing..."
hdiutil convert "$DMG_RW" -format UDZO -imagekey zlib-level=9 -o "$DMG_OUT" >/dev/null

echo "==> DMG ready: $DMG_OUT ($(du -sh "$DMG_OUT" | cut -f1))"
