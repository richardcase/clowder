# muxy M6b — Reproducible libghostty Build Script Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the manual, doc-only libghostty build recipe into a committed, reproducible
`scripts/build-libghostty.sh` that clones ghostty at the exact pin, applies the Darwin static-lib
patch, builds with zig, and installs `ghostty-internal.a` + `ghostty.h` into the repo — so the
vendored `.a` is reproducible from source and CI (M6d) can invoke it.

**Architecture:** Two committed files — `scripts/libghostty-darwin-install.patch` (the exact `build.zig`
Darwin-install patch) and `scripts/build-libghostty.sh` (clone-at-pin → apply patch → `zig build` →
copy outputs). The 189 MB `.a` stays gitignored (`macos/.gitignore` already ignores `vendor/`); the
committed `macos/Sources/GhosttyKit/include/ghostty.h` is regenerated (a no-op diff at the same pin).
The ghostty clone lives in a gitignored cache dir so re-runs are cheap.

**Tech Stack:** bash, git, zig 0.16.0, full Xcode (Metal shader compiler). Spec:
`docs/superpowers/specs/2026-07-30-muxy-m6-packaging-design.md` (§M6b).

## Global Constraints

- **Ghostty pin:** `2de5e7d38e1354759211722a8687c0815d2cf02c` (ghostty 1.3.2-dev, `minimum_zig_version`
  0.16.0). The script MUST build this exact commit.
- **Build recipe (verified in the M0c-2 spike):** `SDKROOT="$(xcrun --show-sdk-path)" zig build
  -Dapp-runtime=none -Demit-xcframework=false`, after applying the Darwin static-lib patch. Produces
  `zig-out/lib/ghostty-internal.a` (~189 MB) + `zig-out/include/ghostty.h`.
- **Toolchain requirements:** zig `0.16.0` on PATH; **full Xcode** selected (`xcode-select -p` →
  Xcode.app; `xcrun metal --version` works — the Metal shader compiler is CLT-absent and blocks the
  build). The script must check these and fail with a clear message if missing.
- **Outputs:** copy `ghostty-internal.a` → `macos/vendor/libghostty/ghostty-internal.a` (gitignored via
  `macos/.gitignore`'s `vendor/`); copy `ghostty.h` → `macos/Sources/GhosttyKit/include/ghostty.h`
  (COMMITTED — at the same pin it's a byte-identical no-op).
- **Idempotent + re-runnable:** clone/reuse ghostty in a gitignored cache (`.cache/ghostty`), reset it
  to a clean pinned checkout each run (so a re-apply of the patch never doubles up), rebuild, copy.
- **Scope: M6b only** — the script + patch + a toolchain doc. No CI wiring (M6d), no changes to
  `macos/Package.swift` or any Swift/Rust source. `set -euo pipefail`. Prefix cargo/swift-free.

---

## Task 1: `build-libghostty.sh` + the Darwin patch (+ toolchain doc)

**Files:**
- Create: `scripts/libghostty-darwin-install.patch`
- Create: `scripts/build-libghostty.sh` (executable)
- Create: `docs/building-libghostty.md` (toolchain + usage note)
- Modify: `.gitignore` (add `/.cache/`)

**Interfaces:**
- Produces: a re-runnable `scripts/build-libghostty.sh` that regenerates
  `macos/vendor/libghostty/ghostty-internal.a` + `macos/Sources/GhosttyKit/include/ghostty.h` from the
  pinned ghostty source. No code interface; consumed by developers + M6d CI.

- [ ] **Step 1: Create the Darwin static-lib patch.** The pinned `build.zig` guards the macOS static-lib
install off (`if (!config.target.result.os.tag.isDarwin())` around lines 195–204). This patch adds a
Darwin branch that installs the header + `ghostty-internal.a`. Create
`scripts/libghostty-darwin-install.patch` with EXACTLY this content (it is a `git apply -p1` unified
diff against ghostty's `build.zig` at the pin — the context lines are verbatim from that commit):

```diff
--- a/build.zig
+++ b/build.zig
@@ -192,7 +192,10 @@
         // We shouldn't have this guard but we don't currently
         // build on macOS this way ironically so we need to fix that.
-        if (!config.target.result.os.tag.isDarwin()) {
+        if (config.target.result.os.tag.isDarwin()) {
+            lib_static.installHeader();
+            lib_static.install("ghostty-internal.a");
+        } else if (!config.target.result.os.tag.isDarwin()) {
             lib_shared.installHeader(); // Only need one header
             if (config.target.result.os.tag == .windows) {
                 lib_shared.install("ghostty-internal.dll");
```

(Rationale: `GhosttyLib` exposes both `install(name)` and `installHeader()`; on Darwin we install the
header once and the static `ghostty-internal.a`, mirroring the else-branch's `.a` install. This is the
exact edit the M0c-2 spike used to produce the 189 MB lib + `ghostty.h`.)

- [ ] **Step 2: Create the build script.** Create `scripts/build-libghostty.sh`:

```bash
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
  *Xcode.app*) : ;;
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
```

Make it executable:
```bash
chmod +x /Users/richard/code/muxy/scripts/build-libghostty.sh
```

- [ ] **Step 3: Add the cache dir to `.gitignore`.** Append to `/Users/richard/code/muxy/.gitignore`:

```
/.cache/
```

- [ ] **Step 4: Write the toolchain doc.** Create `docs/building-libghostty.md`:

```markdown
# Building libghostty

`macos/Sources/GhosttyKit/include/ghostty.h` is committed; the 189 MB
`macos/vendor/libghostty/ghostty-internal.a` is gitignored and built from source by
`scripts/build-libghostty.sh`.

## Requirements

- **zig 0.16.0** (ghostty's `minimum_zig_version`): `brew install zig` (or the matching release).
- **Full Xcode** (not just Command Line Tools) — the Metal shader compiler (`xcrun metal`) is
  Xcode-only and the ghostty renderer needs it:
  ```
  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  sudo xcodebuild -license accept
  xcrun metal --version   # must succeed
  ```

## Build

```
scripts/build-libghostty.sh
```

This clones ghostty at the pin `2de5e7d38e1354759211722a8687c0815d2cf02c` into `.cache/ghostty`
(gitignored), applies `scripts/libghostty-darwin-install.patch` (adds the macOS static-lib install
that ghostty's `build.zig` otherwise guards off), runs
`SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false`, and copies
`ghostty-internal.a` + `ghostty.h` into the repo. The build is heavy (Metal shaders + ~189 MB, several
minutes); the ghostty clone is cached for cheap re-runs.

## Bumping the pin

Change `GHOSTTY_PIN` in `scripts/build-libghostty.sh`, re-run it, rebuild the app
(`cd macos && swift build`), and commit the regenerated `ghostty.h` (the `.a` stays gitignored). If
ghostty's `build.zig` static-lib block moved, regenerate `scripts/libghostty-darwin-install.patch`.
```

- [ ] **Step 5: Run the script end-to-end (the hard gate).**

Run:
```bash
/Users/richard/code/muxy/scripts/build-libghostty.sh
```
Expected: toolchain checks pass, ghostty fetches at the pin, the patch applies cleanly (`git apply`
prints the applied hunk), `zig build` completes (this takes SEVERAL MINUTES — do not abort), and the
script prints the two installed paths with the `.a` around 189 MB.

- [ ] **Step 6: Verify reproducibility — the regenerated header matches the committed one.**

Run:
```bash
cd /Users/richard/code/muxy && git diff --stat -- macos/Sources/GhosttyKit/include/ghostty.h
```
Expected: **no diff** (empty output) — regenerating at the same pin yields the byte-identical committed
`ghostty.h`. (If it differs, the pin/patch/toolchain drifted — investigate before proceeding.)

- [ ] **Step 7: Verify the freshly-built `.a` links — rebuild the app.**

Run:
```bash
cd /Users/richard/code/muxy/macos && swift build 2>&1 | tail -3
```
Expected: `Build complete!` — the reproduced `ghostty-internal.a` links MuxyApp exactly like the
manually-built one (proves the from-source `.a` is valid, not just present).

- [ ] **Step 8: Confirm nothing unwanted is staged** (the `.a` + cache stay ignored).

Run:
```bash
cd /Users/richard/code/muxy && git status --short
```
Expected: only the new tracked files (`scripts/libghostty-darwin-install.patch`,
`scripts/build-libghostty.sh`, `docs/building-libghostty.md`, `.gitignore`) — NOT
`macos/vendor/libghostty/ghostty-internal.a`, NOT `.cache/`, and (per Step 6) NOT a modified
`ghostty.h`.

- [ ] **Step 9: Commit**

```bash
git add scripts/libghostty-darwin-install.patch scripts/build-libghostty.sh docs/building-libghostty.md .gitignore
git commit -m "feat(build): scripts/build-libghostty.sh reproduces libghostty from the pinned source"
```

---

## Self-Review Notes (author)

- **Spec §M6b coverage:** the committed build script capturing the recipe → Steps 2–3; the exact
  `build.zig` Darwin patch (the doc-only recipe made concrete) → Step 1; toolchain documentation →
  Step 4; the `.a` stays gitignored + `ghostty.h` committed → Steps 6/8 (`macos/.gitignore` already
  ignores `vendor/`); "the build step M6d's CI invokes" → the script is CI-callable (no interactive
  bits; toolchain checks fail loudly). Spec §Testing "build-libghostty.sh reproduces the vendored `.a`
  (checked by a successful downstream link)" → Steps 5–7.
- **The patch is exact, not approximate:** reconstructed from the pinned `build.zig` (lines 195–204)
  with verbatim context; `git apply` at a FIXED pin is deterministic (the source never changes), so the
  line-numbered diff applies cleanly every run.
- **Idempotency:** `git fetch --depth 1` + `checkout --force FETCH_HEAD` + `clean -fdx` +
  `checkout -- .` gives a pristine pinned tree each run before re-applying the patch, so repeated runs
  never double-apply or accumulate `zig-out`.
- **No placeholders:** every command is concrete; the only runtime variability (SDK path, du size) is
  computed in-script.
- **Deferred / out of scope:** wiring this into CI with caching (M6d); bumping the pin (documented, not
  performed); a universal/arm64+x86_64 build (the script builds the runner's native arch, matching the
  current vendored `.a`).
- **Heavy verification caveat:** Steps 5–7 require a full ghostty build (Metal + ~189 MB, several
  minutes) with zig 0.16.0 + full Xcode present — both confirmed available in this environment.
