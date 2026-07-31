#!/usr/bin/env bash
# Reproducibly build libghostty (ghostty-internal.a + ghostty.h) from the pinned ghostty source,
# and install them into the repo. Requires zig 0.16.0 + full Xcode (Metal shader compiler).
#
# Usage: scripts/build-libghostty.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY_PIN="2de5e7d38e1354759211722a8687c0815d2cf02c"
GHOSTTY_REPO="https://github.com/ghostty-org/ghostty"
CACHE="$ROOT/.cache/ghostty"
PATCH="$ROOT/scripts/libghostty-darwin-install.patch"
VENDOR_A="$ROOT/macos/vendor/libghostty/ghostty-internal.a"
HEADER_DST="$ROOT/macos/Sources/GhosttyKit/include/ghostty.h"

echo "==> Checking toolchain"
command -v zig >/dev/null || { echo "error: zig not on PATH (need 0.16.0)" >&2; exit 1; }
ZIG_VER="$(zig version)"
[ "$ZIG_VER" = "0.16.0" ] || echo "warning: zig $ZIG_VER (recipe verified with 0.16.0)"
xcodesel="$(xcode-select -p 2>/dev/null || true)"
case "$xcodesel" in
  # Match plain, versioned, and beta Xcode installs (e.g. Xcode.app, Xcode_16.4.app, Xcode-beta.app);
  # the `xcrun metal` check below is the authoritative CLT-vs-full-Xcode gate.
  *Xcode*.app*) : ;;
  *) echo "error: full Xcode required (xcode-select -p = '$xcodesel'); the Metal shader compiler is not in CLT" >&2; exit 1 ;;
esac
xcrun metal --version >/dev/null 2>&1 || { echo "error: 'xcrun metal' unavailable — full Xcode + accepted license needed" >&2; exit 1; }

echo "==> Preparing ghostty checkout at $GHOSTTY_PIN"
if [ ! -d "$CACHE/.git" ]; then
  mkdir -p "$CACHE"
  git -C "$CACHE" init -q
  git -C "$CACHE" remote add origin "$GHOSTTY_REPO" 2>/dev/null || true
fi
git -C "$CACHE" fetch --depth 1 origin "$GHOSTTY_PIN"
git -C "$CACHE" checkout -q --force FETCH_HEAD
git -C "$CACHE" clean -fdxq            # drop prior zig-out / patched build.zig for a clean, re-appliable tree
git -C "$CACHE" checkout -q -- .       # restore any tracked files (e.g. a previously-patched build.zig)

echo "==> Applying Darwin static-lib patch"
git -C "$CACHE" apply --verbose "$PATCH"

echo "==> Building libghostty (zig build — this is heavy: Metal shaders + ~189 MB, several minutes)"
( cd "$CACHE" && SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false )

OUT_A="$CACHE/zig-out/lib/ghostty-internal.a"
OUT_H="$CACHE/zig-out/include/ghostty.h"
[ -f "$OUT_A" ] || { echo "error: build did not produce $OUT_A" >&2; exit 1; }
[ -f "$OUT_H" ] || { echo "error: build did not produce $OUT_H" >&2; exit 1; }

echo "==> Installing outputs"
mkdir -p "$(dirname "$VENDOR_A")" "$(dirname "$HEADER_DST")"
cp "$OUT_A" "$VENDOR_A"
cp "$OUT_H" "$HEADER_DST"

echo "==> Done"
echo "    $VENDOR_A ($(du -h "$VENDOR_A" | cut -f1))"
echo "    $HEADER_DST"
