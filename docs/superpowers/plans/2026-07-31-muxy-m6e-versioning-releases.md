# muxy M6e — Versioning + Releases Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A single source of truth for the app version (the top-level `VERSION` file), a
`scripts/set-version.sh` that propagates it into the Rust crate versions (retiring `0.0.0`), and a
`release.yml` workflow that, on a `vX.Y.Z` tag, builds the app and publishes a GitHub Release with the
unsigned `Muxy.app` attached.

**Architecture:** `VERSION` stays canonical (already read by `build-app.sh` → Info.plist). Rust
versions are centralized via `[workspace.package] version` + `version.workspace = true` in every crate;
`set-version.sh` writes `VERSION` and stamps that workspace version + refreshes `Cargo.lock`.
`release.yml` reuses the M6d build recipe (Xcode 16 → brew zig → Rust → cached libghostty →
`build-app.sh`), verifies the tag matches `VERSION`, zips the `.app`, and creates the Release.

**Tech Stack:** bash, Cargo workspace versioning, GitHub Actions. Spec:
`docs/superpowers/specs/2026-07-30-muxy-m6-packaging-design.md` (§M6e).

## Global Constraints

- **`VERSION` (repo root) is the single source of truth.** Current value `0.1.0`. `set-version.sh`
  propagates it; nothing hand-edits crate versions after this.
- **Retire `0.0.0`:** all 7 crates move to `version.workspace = true` with `[workspace.package] version`
  in the root `Cargo.toml` (= the `VERSION` value). `Cargo.lock` MUST be regenerated + committed so
  M6d's `cargo test --locked` stays green.
- **Release is UNSIGNED** (M6c deferred): the `.app` is zipped with `ditto` and attached as-is; a
  downloaded unsigned app is Gatekeeper-quarantined (right-click→Open) — documented, not fixed here.
- **`release.yml` verification is a real Actions run on a real `vX.Y.Z` tag** — it creates a real
  GitHub Release. Cannot be verified locally. Tag/verification approach is a human decision (see the
  execution note); the workflow itself is authored + `actionlint`-clean first.
- **Reuse the M6d recipe:** `release.yml` duplicates the ci.yml build steps (Xcode 16, `brew install
  zig`, Rust, `actions/cache` libghostty, `build-libghostty.sh` on miss, `build-app.sh`). Keeping them
  in sync is a documented carry-forward (could extract a reusable workflow later) — do NOT refactor the
  just-merged `ci.yml` in this slice.
- **Scope: M6e only** — versioning tooling + release workflow. No signing/notarize/DMG (M6c), no
  Homebrew (M6f). `set -euo pipefail`. Prefix cargo with `source "$HOME/.cargo/env" && `.

---

## Task 1: Version single-source (`set-version.sh` + retire `0.0.0`)

**Files:**
- Modify: root `Cargo.toml` (add `[workspace.package] version`)
- Modify: `crates/*/Cargo.toml` (all 7 → `version.workspace = true`)
- Modify: `Cargo.lock` (regenerated)
- Create: `scripts/set-version.sh` (executable)
- Modify: `docs/building-libghostty.md`? No — Create: `docs/versioning.md`

**Interfaces:**
- Produces: `scripts/set-version.sh <X.Y.Z>` — writes `VERSION`, sets `[workspace.package] version`, and
  refreshes `Cargo.lock`. With no arg, re-propagates the current `VERSION`.

- [ ] **Step 1: Centralize the Rust version.** In the root `Cargo.toml`, add a `[workspace.package]`
section (after `[workspace]`/`members`):

```toml
[workspace.package]
version = "0.1.0"
edition = "2021"
```

Then in EACH of the 7 `crates/*/Cargo.toml`, change the package version (and, to keep edition
centralized too, edition) to inherit. Change each crate's `[package]` block from:

```toml
[package]
name = "muxy-daemon"
version = "0.0.0"
edition = "2021"
```
to:
```toml
[package]
name = "muxy-daemon"
version.workspace = true
edition.workspace = true
```

(Do this for all 7: muxy-client, muxy-config, muxy-daemon, muxy-hook, muxy-proto, muxy-vt,
muxy-workspace. Keep each crate's `name` and any other fields unchanged.)

- [ ] **Step 2: Regenerate `Cargo.lock` and confirm the build still works.**

Run:
```bash
source "$HOME/.cargo/env" && cargo update --workspace && cargo build --workspace
```
Expected: `Cargo.lock` now records the workspace crates at `0.1.0` (not `0.0.0`); the build succeeds.
Verify:
```bash
grep -A1 'name = "muxy-daemon"' Cargo.lock | grep 'version = "0.1.0"'
cargo metadata --no-deps --format-version 1 | grep -o '"version":"0.1.0"' | head -1
```
Expected: both show `0.1.0`.

- [ ] **Step 3: Write `set-version.sh`.** Create `scripts/set-version.sh`:

```bash
#!/usr/bin/env bash
# Set the muxy version everywhere from a single source. Writes the top-level VERSION file and the
# Cargo workspace version, then refreshes Cargo.lock. The macOS bundle version flows from VERSION via
# scripts/build-app.sh (Info.plist), and SwiftPM versioning comes from the git tag.
#
# Usage: scripts/set-version.sh <X.Y.Z>   (or no arg to re-propagate the current VERSION)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "$#" -ge 1 ]; then
  VERSION="$1"
else
  VERSION="$(tr -d '[:space:]' < VERSION)"
fi

# Validate semver X.Y.Z (optionally a -prerelease suffix).
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) : ;;
  *) echo "error: version '$VERSION' is not X.Y.Z" >&2; exit 1 ;;
esac

echo "==> Setting version $VERSION"
printf '%s\n' "$VERSION" > VERSION

# Update [workspace.package] version in the root Cargo.toml (the single Rust version source).
# Only the line inside the [workspace.package] section is touched.
awk -v v="$VERSION" '
  /^\[workspace\.package\]/ { inwp=1 }
  inwp && /^\[/ && !/^\[workspace\.package\]/ { inwp=0 }
  inwp && /^version[[:space:]]*=/ { print "version = \"" v "\""; next }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

echo "==> Refreshing Cargo.lock"
( source "$HOME/.cargo/env" 2>/dev/null || true; cargo update --workspace >/dev/null )

echo "==> Version is now $VERSION"
grep '^version' <(awk '/^\[workspace\.package\]/{f=1} f&&/^version/{print;exit}' Cargo.toml)
```

Make it executable:
```bash
chmod +x /Users/richard/code/muxy/scripts/set-version.sh
```

- [ ] **Step 4: Verify `set-version.sh` is idempotent + drives the bundle version.**

Run:
```bash
scripts/set-version.sh 0.1.0
git diff --stat           # expect NO changes (already 0.1.0 — idempotent)
scripts/set-version.sh 0.2.0
grep -A2 '\[workspace.package\]' Cargo.toml | grep '0.2.0'
cat VERSION               # 0.2.0
grep -A1 'name = "muxy-proto"' Cargo.lock | grep '0.2.0'
# revert to the real version:
scripts/set-version.sh 0.1.0
```
Expected: `0.2.0` round-trips through `VERSION` + `Cargo.toml` + `Cargo.lock`, then reverts cleanly to
`0.1.0`. Confirm `scripts/build-app.sh`'s Info.plist would use `VERSION` (already the case:
`build-app.sh` reads `VERSION`).

- [ ] **Step 5: Run the whole workspace suite (no regressions from the version change).**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked 2>&1 | grep -E 'test result|error' | tail -20`
Expected: all suites `0 failed`, AND `--locked` succeeds (proving `Cargo.lock` is in sync — this is
exactly what M6d's CI runs).

- [ ] **Step 6: Write the versioning doc.** Create `docs/versioning.md`:

```markdown
# Versioning & releases

The version lives in one place: the top-level `VERSION` file (currently `0.1.0`).

- **Rust crates** inherit it via `[workspace.package] version` in the root `Cargo.toml`
  (`version.workspace = true` in each crate).
- **The macOS app** (`Muxy.app` Info.plist `CFBundleShortVersionString`/`CFBundleVersion`) reads
  `VERSION` in `scripts/build-app.sh`.

## Bumping the version

```
scripts/set-version.sh 0.2.0     # writes VERSION + the Cargo workspace version + refreshes Cargo.lock
git commit -am "chore: v0.2.0"
git tag v0.2.0
git push && git push --tags      # the v* tag triggers .github/workflows/release.yml
```

## Releasing

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`, which builds the app and publishes a
GitHub Release with the **unsigned** `Muxy.app` (zipped). The tag must match `VERSION` (the workflow
checks this). An unsigned app is Gatekeeper-quarantined on download — right-click → Open (or
`xattr -dr com.apple.quarantine Muxy.app`). Code-signing/notarization + a DMG land in M6c; a Homebrew
cask in M6f.
```

- [ ] **Step 7: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/*/Cargo.toml scripts/set-version.sh docs/versioning.md
git commit -m "chore(release): single-source version (VERSION + workspace.package), set-version.sh, retire 0.0.0"
```

---

## Task 2: `release.yml` — build + publish a GitHub Release on a `vX.Y.Z` tag

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Produces: a workflow that, on a `v*` tag, builds the app (M6d recipe), verifies tag==VERSION, zips
  `Muxy.app`, and creates a GitHub Release with the zip attached.

- [ ] **Step 1: Write the release workflow.** Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags: ['v*']

permissions:
  contents: write   # create the GitHub Release

jobs:
  release:
    name: build + publish (macOS, unsigned)
    runs-on: macos-15
    steps:
      - uses: actions/checkout@v4

      - name: Verify the tag matches VERSION
        run: |
          tag="${GITHUB_REF_NAME#v}"
          file="$(tr -d '[:space:]' < VERSION)"
          echo "tag=$tag VERSION=$file"
          [ "$tag" = "$file" ] || { echo "::error::tag v$tag does not match VERSION $file"; exit 1; }

      - name: Select Xcode (Metal shader compiler needs full Xcode)
        uses: maxim-lobanov/setup-xcode@v1
        with:
          xcode-version: '16'

      - name: Verify toolchain
        run: xcrun metal --version && swift --version

      - name: Install zig (Homebrew — reliable bottle CDN)
        run: brew install zig && zig version

      - name: Install Rust (stable)
        uses: dtolnay/rust-toolchain@stable

      - name: Cache libghostty (built once per pin/patch)
        id: libghostty-cache
        uses: actions/cache@v4
        with:
          path: |
            macos/vendor/libghostty/ghostty-internal.a
            macos/Sources/GhosttyKit/include/ghostty.h
          key: libghostty-${{ runner.os }}-${{ hashFiles('scripts/build-libghostty.sh', 'scripts/libghostty-darwin-install.patch') }}

      - name: Build libghostty (cache miss only)
        if: steps.libghostty-cache.outputs.cache-hit != 'true'
        run: scripts/build-libghostty.sh

      - name: Assemble unsigned Muxy.app
        run: scripts/build-app.sh

      - name: Zip the app bundle
        run: ditto -c -k --keepParent dist/Muxy.app "dist/Muxy-${GITHUB_REF_NAME}-macos.zip"

      - name: Publish GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: dist/Muxy-*-macos.zip
          generate_release_notes: true
          body: |
            Unsigned macOS build of Muxy (`${{ github.ref_name }}`).

            > This build is **not code-signed or notarized** (that's a later milestone). On first
            > launch macOS Gatekeeper will quarantine it — right-click the app → **Open**, or run
            > `xattr -dr com.apple.quarantine Muxy.app`.
```

- [ ] **Step 2: Static-lint the workflow.**

```bash
command -v actionlint >/dev/null || brew install actionlint
actionlint .github/workflows/release.yml
```
Expected: clean.

- [ ] **Step 3: Commit.**

```bash
git add .github/workflows/release.yml
git commit -m "ci: release.yml — publish a GitHub Release with the unsigned Muxy.app on a v* tag"
```

- [ ] **Step 4: Verify on a real tag (real-CI, human-gated).** Per the execution decision, create and
push a `vX.Y.Z` tag matching `VERSION` (e.g. `v0.1.0`), then watch the run and confirm the Release is
created with the zip attached:

```bash
git tag v0.1.0
git push origin v0.1.0
RUN=$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
# poll until completed (see M6d's watcher pattern; use `st`, not the zsh read-only `status`)
gh run view "$RUN" --json conclusion --jq '.conclusion'
gh release view "v0.1.0" --json name,assets --jq '{name, assets: [.assets[].name]}'
```
Expected: the run succeeds; a GitHub Release `v0.1.0` exists with `Muxy-v0.1.0-macos.zip` attached.
(If the decision is a throwaway test tag, use it and delete the tag+release afterward with
`gh release delete <tag> --yes --cleanup-tag`.)

---

## Self-Review Notes (author)

- **Spec §M6e coverage:** single-source `VERSION` → Task 1 (canonical, already Info.plist-wired);
  `set-version.sh` stamping `VERSION`→crate versions → Task 1; retire `0.0.0` → Task 1 (workspace.package
  + `version.workspace`); `release.yml` on a `v*` tag builds via the M6d recipe + publishes a GitHub
  Release with the artifact → Task 2; "signed DMG + Homebrew join once M6c/M6f land" → left as documented
  follow-ups. Spec §Testing "a `v*` tag yields a GitHub Release with the artifact; VERSION flows into
  Info.plist" → Task 2 Step 4 + Task 1 Step 4.
- **`Cargo.lock`/`--locked` interplay:** the version bump regenerates `Cargo.lock`; Task 1 Step 5 runs
  `cargo test --workspace --locked` to prove it's in sync (mirrors M6d's CI), so the version change can't
  silently break CI.
- **release.yml verification honesty:** like M6d, it's only verifiable by a real Actions run on a real
  tag, which creates a real GitHub Release. Task 2 Step 4 is human-gated on the tag choice.
- **Deferred / carry-forward:** the build steps are duplicated between `ci.yml` and `release.yml` —
  extract a reusable workflow / composite action later (not worth re-touching the just-merged `ci.yml`
  now); signing/notarize/DMG (M6c) replaces the plain zip; Homebrew cask auto-bump (M6f) hooks the
  release. SwiftPM has no in-repo version (git-tag driven) — intentionally not stamped.
