# clowder M6c — Codesign → Notarize → Staple → DMG Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Now that an Apple Developer ID exists, upgrade the release pipeline from an unsigned zip to a
**signed → notarized → stapled DMG**. Add committed signing/packaging scripts and wire them into
`release.yml`, **gated on the signing secret** so forks / an unconfigured repo still publish an unsigned
build. This is the pivot that also unblocks M6f (Homebrew cask).

**Architecture:** `build-app.sh` stays signing-free (keeps `ci.yml` and fork builds working). Two new
scripts operate on the assembled `dist/Clowder.app`: `sign-app.sh` (inner-first hardened-runtime
`codesign`) and `package-dmg.sh` (`hdiutil` DMG → notarize → staple). `release.yml` detects the signing
secret, imports the Developer ID cert into a temp keychain, runs the two scripts, and attaches the DMG;
with no secret it falls through to the existing `ditto`-zip path.

**Tech Stack:** bash (macOS `/bin/bash` 3.2 — empty-array expansion needs the `${a[@]+"${a[@]}"}` idiom),
`codesign`, `xcrun notarytool`/`stapler`, `hdiutil`, GitHub Actions. Spec:
`docs/superpowers/specs/2026-07-30-muxy-m6-packaging-design.md` (§M6c).

## Global Constraints

- **libghostty is a static `.a`** linked into the single `clowder-app` Mach-O — there are **no dylibs or
  frameworks** to sign. Signing targets: the 3 bundled binaries in `Contents/Resources/bin/`, the app exe
  in `Contents/MacOS/`, then the `.app` bundle — signed **inner-first**.
- **Hardened runtime + secure timestamp + Developer ID on every executable**, or notarization rejects the
  bundle. `--options runtime --timestamp`.
- **Do NOT use `codesign --deep`** to sign (deprecated / unreliable for non-standard nesting). Sign each
  item explicitly; `--deep` is used only in `--verify`.
- **Entitlements must be comment-free** — `codesign`'s AMFI XML parser errors on XML comments. Keep
  `macos/clowder-app.entitlements` a bare `<dict/>`; document exceptions in this plan, not in the file.
- **Signing is secret-gated:** no `CODESIGN_P12_BASE64` secret → unsigned zip path (forks/PRs unaffected).
  `ci.yml` is **untouched** (still uploads the unsigned artifact).
- **Notarization creds are undecided:** support both — App Store Connect **API key** (default) and
  **Apple ID + app password** — auto-selected by whichever env set is fully present.
- Scope: M6c only. No Homebrew (M6f). `set -euo pipefail`.

---

## Task 1: Hardened-runtime entitlements — `macos/clowder-app.entitlements`

- [x] Create a bare `<dict/>` plist (no comments). A terminal that exec's its own signed children and
  renders via Metal needs **no** hardened-runtime exception. Add one only if notarization/launch fails:
  `com.apple.security.cs.disable-library-validation` (loads 3rd-party dylibs),
  `com.apple.security.cs.allow-jit` / `...allow-unsigned-executable-memory` (JIT). None expected.

## Task 2: `scripts/sign-app.sh`

- [x] Sign `dist/Clowder.app` (arg-overridable) inner-first. Env: `CODESIGN_IDENTITY` (default
  `"Developer ID Application"`; `"-"` = ad-hoc smoke test, no timestamp), `CODESIGN_KEYCHAIN` (CI temp
  keychain). Sign the 3 `Resources/bin` binaries with `--options runtime --timestamp`; sign the app exe +
  bundle additionally with `--entitlements macos/clowder-app.entitlements`. Verify with
  `codesign --verify --deep --strict --verbose=2`.
- **Note:** the bundled binaries live in `Contents/Resources/bin/` (M6a's layout, chosen so the daemon
  resolves `clowder-hook` as an exe-sibling). This is non-standard nesting but signs + notarizes fine as
  long as each is individually signed (they are). If a future Gatekeeper change objects, move them to
  `Contents/MacOS/` or `Contents/Helpers/` and update `agent.rs` resolution.

## Task 3: `scripts/package-dmg.sh`

- [x] From a **signed** `dist/Clowder.app`, stage it + an `/Applications` symlink (drag-install layout,
  via `ditto` to preserve the signature), `hdiutil create -format UDZO` → `dist/Clowder-<VERSION>-macos.dmg`,
  `codesign` the DMG, then notarize. Credential auto-select (first fully-present set wins; omit all to
  build+sign only): `NOTARY_PROFILE` (keychain profile) › `NOTARY_KEY`+`NOTARY_KEY_ID`+`NOTARY_ISSUER`
  (API key, `NOTARY_KEY` = `.p8` path) › `NOTARY_APPLE_ID`+`NOTARY_PASSWORD`+`NOTARY_TEAM_ID`. Then
  `xcrun stapler staple` + validate + `spctl -a -t open`.
- **Note:** the DMG is notarized/stapled, not the inner `.app`. Online Gatekeeper still accepts an app
  drag-copied out of the DMG; a two-pass flow (also staple the `.app`) is an optional robustness upgrade.

## Task 4: `release.yml` signed path (secret-gated)

- [x] `build-app.sh` always runs. A **Detect signing secrets** step emits `steps.signing.outputs.enabled`
  (`CODESIGN_P12_BASE64` present?). When true: import the cert into a `$RUNNER_TEMP` keychain
  (`security create-keychain` → `set-keychain-settings -lut` → `unlock` → `import -f pkcs12` →
  `set-key-partition-list -S apple-tool:,apple:,codesign:` → `list-keychain -s`), export `CODESIGN_KEYCHAIN`,
  run `sign-app.sh`, decode `NOTARY_KEY_BASE64` → `NOTARY_KEY`, run `package-dmg.sh`, publish the DMG; a
  final `always()` step deletes the keychain. When false: existing `ditto` zip + unsigned-body publish.

## Task 5: Docs + version

- [x] README: replace the unsigned/`xattr` notes with the signed-DMG story + local signing usage.
- [x] Spec M6c section: mark "now built (M6c)".
- [ ] `VERSION` bump for the first signed release (confirm value with maintainer; annotated tag `vX.Y.Z`).

---

## Required GitHub Actions secrets (maintainer sets before the signed release)

| Secret | Purpose |
|---|---|
| `CODESIGN_P12_BASE64` | base64 of the exported **Developer ID Application** cert+key `.p12` (gates signing) |
| `CODESIGN_P12_PASSWORD` | password protecting that `.p12` |
| `CODESIGN_IDENTITY` | e.g. `Developer ID Application: Richard Case (TEAMID)` |
| `KEYCHAIN_PASSWORD` | arbitrary password for the ephemeral CI keychain |
| **API-key notary (default)** | `NOTARY_KEY_BASE64` (base64 of `AuthKey_*.p8`), `NOTARY_KEY_ID`, `NOTARY_ISSUER` |
| **or Apple-ID notary** | `NOTARY_APPLE_ID`, `NOTARY_PASSWORD` (app-specific), `NOTARY_TEAM_ID` |

## Local validation (maintainer — needs the real cert + Apple account)

```sh
scripts/build-app.sh
CODESIGN_IDENTITY="Developer ID Application: … (TEAMID)" scripts/sign-app.sh
# one-time: store notary creds in the keychain (API key shown)
xcrun notarytool store-credentials clowder-notary \
  --key AuthKey_XXXX.p8 --key-id XXXXXXXXXX --issuer <issuer-uuid>
NOTARY_PROFILE=clowder-notary CODESIGN_IDENTITY="Developer ID Application: … (TEAMID)" \
  scripts/package-dmg.sh
# expect: notarytool "Accepted", `stapler validate` ok, `spctl` accepts the DMG.
# then mount the DMG on a clean Mac → Clowder launches with no Gatekeeper block; daemon auto-spawns.
```

## Verification gate

Agent smoke-test (no cert): ad-hoc sign a bundle → `codesign --verify --deep --strict` passes; `hdiutil`
DMG builds; `actionlint` clean; `cargo test --workspace` + `swift test` green. End-to-end notarized DMG +
CI signed release are maintainer steps (real Developer ID required).
