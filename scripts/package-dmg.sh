#!/usr/bin/env bash
# Package a signed Clowder.app into a notarized + stapled DMG (M6c). Run scripts/sign-app.sh first.
#
# Usage: scripts/package-dmg.sh [app-path] [out-dmg]
#   defaults: dist/Clowder.app  ->  dist/Clowder-<VERSION>-macos.dmg
# Env:
#   CODESIGN_IDENTITY   identity used to sign the DMG itself (default "Developer ID Application").
#   Notarization credentials — first set that is fully present wins; omit ALL to build+sign only
#   (no notarize/staple), which is the offline smoke-test path:
#     NOTARY_PROFILE                                     keychain profile (notarytool store-credentials)
#     NOTARY_KEY + NOTARY_KEY_ID + NOTARY_ISSUER         App Store Connect API key (NOTARY_KEY = .p8 path)
#     NOTARY_APPLE_ID + NOTARY_PASSWORD + NOTARY_TEAM_ID Apple ID + app-specific password
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
APP="${1:-$ROOT/dist/Clowder.app}"
DMG="${2:-$ROOT/dist/Clowder-$VERSION-macos.dmg}"
IDENTITY="${CODESIGN_IDENTITY:-Developer ID Application}"

[ -d "$APP" ] || { echo "no app bundle at: $APP" >&2; exit 1; }

echo "==> Building DMG $DMG"
STAGE="$(mktemp -d)"
ditto "$APP" "$STAGE/$(basename "$APP")"     # ditto preserves the code signature + all metadata
ln -s /Applications "$STAGE/Applications"    # classic drag-to-Applications layout
rm -f "$DMG"
hdiutil create -volname Clowder -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "==> Signing DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

# Select notarytool credentials by whichever env set is fully present.
notary=()
if [ -n "${NOTARY_PROFILE:-}" ]; then
  notary=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${NOTARY_KEY:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then
  notary=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ]; then
  notary=(--apple-id "$NOTARY_APPLE_ID" --password "$NOTARY_PASSWORD" --team-id "$NOTARY_TEAM_ID")
fi

if [ ${#notary[@]} -eq 0 ]; then
  echo "==> No notarization credentials set — built + signed DMG only (skipping notarize/staple)."
  echo "    $DMG"
  exit 0
fi

echo "==> Submitting to Apple notary service (can take a few minutes)"
xcrun notarytool submit "$DMG" "${notary[@]}" --wait

echo "==> Stapling ticket"
xcrun stapler staple "$DMG"

echo "==> Verifying"
xcrun stapler validate "$DMG"
spctl -a -t open --context context:primary-signature -vv "$DMG"
echo "==> Notarized DMG: $DMG"
