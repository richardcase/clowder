#!/usr/bin/env bash
# Sign Clowder.app for Developer ID distribution (M6c): hardened runtime, inner-first, secure timestamp.
# Signs each nested binary explicitly (inner-first), then the app bundle — NOT `codesign --deep`, which
# is deprecated and unreliable for bundles with executables outside the standard nesting locations.
#
# Usage: scripts/sign-app.sh [app-path]        (default: dist/Clowder.app)
# Env:
#   CODESIGN_IDENTITY   signing identity (default "Developer ID Application").
#                       Use "-" for an ad-hoc smoke test (no real cert, no secure timestamp).
#   CODESIGN_KEYCHAIN   optional keychain to search for the identity (e.g. a CI temp keychain).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:-$ROOT/dist/Clowder.app}"
IDENTITY="${CODESIGN_IDENTITY:-Developer ID Application}"
ENTITLEMENTS="$ROOT/macos/clowder-app.entitlements"

[ -d "$APP" ] || { echo "no app bundle at: $APP" >&2; exit 1; }
[ -f "$ENTITLEMENTS" ] || { echo "missing entitlements: $ENTITLEMENTS" >&2; exit 1; }

# A real Developer ID must carry a secure timestamp; the ad-hoc identity ("-") cannot.
ts=(--timestamp)
kc=()
if [ "$IDENTITY" = "-" ]; then
  ts=(--timestamp=none)
  echo "==> Ad-hoc signing (smoke test only — not distributable)"
fi
[ -n "${CODESIGN_KEYCHAIN:-}" ] && kc=(--keychain "$CODESIGN_KEYCHAIN")

# sign <file> [extra codesign args...]   (${kc[@]+...} idiom: safe under `set -u` in bash 3.2 when empty)
sign() {
  local f="$1"; shift
  codesign --force --options runtime "${ts[@]}" ${kc[@]+"${kc[@]}"} --sign "$IDENTITY" "$@" "$f"
}

echo "==> Signing nested binaries (inner-first)"
for bin in clowder-hook clowder clowder-daemon; do
  sign "$APP/Contents/Resources/bin/$bin"
done

echo "==> Signing app executable + bundle (with entitlements)"
sign "$APP/Contents/MacOS/clowder-app" --entitlements "$ENTITLEMENTS"
sign "$APP"                            --entitlements "$ENTITLEMENTS"

echo "==> Verifying"
codesign --verify --deep --strict --verbose=2 "$APP"
echo "==> Signed: $APP"
