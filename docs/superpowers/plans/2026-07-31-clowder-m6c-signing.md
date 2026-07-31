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
  frameworks** to sign. Signing targets, all in `Contents/MacOS/`: the 3 bundled binaries
  (`clowder-daemon`/`clowder`/`clowder-hook`), the app exe (`clowder-app`), then the `.app` bundle —
  signed **inner-first**.
- **Hardened runtime + secure timestamp + Developer ID on every executable**, or notarization rejects the
  bundle. `--options runtime --timestamp`.
- **Do NOT use `codesign --deep`** to sign (deprecated / unreliable for non-standard nesting). Sign each
  item explicitly; `--deep` is used only in `--verify`.
- **Entitlements must be comment-free** — `codesign`'s AMFI XML parser errors on XML comments. Keep
  `macos/clowder-app.entitlements` a bare `<dict/>`; document exceptions in this plan, not in the file.
- **Signing is gated on the `DOPPLER_IDENTITY_ID` repo variable** → unset ⇒ unsigned zip path (forks/PRs
  unaffected). Signing material is fetched from Doppler over GitHub OIDC (no GitHub secrets); see
  [`docs/code-signing.md`](../../code-signing.md). `ci.yml` is **untouched** (still uploads the unsigned artifact).
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
  keychain). Sign the 3 `Contents/MacOS/` binaries with `--options runtime --timestamp`; sign the app exe +
  bundle additionally with `--entitlements macos/clowder-app.entitlements`. Verify with
  `codesign --verify --deep --strict --verbose=2`.
- **Note:** all four executables live in `Contents/MacOS/` — the standard nesting location for a bundle's
  main exe plus additional command-line tools. The three Rust binaries stay co-located so the daemon
  resolves `clowder-hook` as an exe-sibling of `clowder-daemon` (`agent.rs`) and the app resolves
  `clowder` next to its own executable. (Originally `Contents/Resources/bin/`; moved to `MacOS/` because
  `Resources/` is for non-code and `codesign` seals it as a resource rather than first-class nested code.)

## Task 3: `scripts/package-dmg.sh`

- [x] From a **signed** `dist/Clowder.app`, stage it + an `/Applications` symlink (drag-install layout,
  via `ditto` to preserve the signature), `hdiutil create -format UDZO` → `dist/Clowder-<VERSION>-macos.dmg`,
  `codesign` the DMG, then notarize. Credential auto-select (first fully-present set wins; omit all to
  build+sign only): `NOTARY_PROFILE` (keychain profile) › `NOTARY_KEY`+`NOTARY_KEY_ID`+`NOTARY_ISSUER`
  (API key, `NOTARY_KEY` = `.p8` path) › `NOTARY_APPLE_ID`+`NOTARY_PASSWORD`+`NOTARY_TEAM_ID`. Then
  `xcrun stapler staple` + validate + `spctl -a -t open`.
- **Note:** the DMG is notarized/stapled, not the inner `.app`. Online Gatekeeper still accepts an app
  drag-copied out of the DMG; a two-pass flow (also staple the `.app`) is an optional robustness upgrade.

## Task 4: `release.yml` signed path (gated on `DOPPLER_IDENTITY_ID` variable)

- [x] `build-app.sh` always runs. When the repo Variable `DOPPLER_IDENTITY_ID` is set: fetch the signing
  secrets from Doppler over OIDC (`dopplerhq/secrets-fetch-action`, `auth-method: oidc`, `id-token: write`),
  import the cert into a `$RUNNER_TEMP` keychain (`security create-keychain` → `set-keychain-settings -lut`
  → `unlock` → `import -f pkcs12` → `set-key-partition-list -S apple-tool:,apple:,codesign:` →
  `list-keychain -s`), export `CODESIGN_KEYCHAIN`, run `sign-app.sh`, decode `NOTARY_KEY_BASE64` →
  `NOTARY_KEY`, run `package-dmg.sh`, publish the DMG; a final `always()` step deletes the keychain. When
  unset: existing `ditto` zip + unsigned-body publish. (Superseded the original GHA-secrets wiring — see
  [`docs/code-signing.md`](../../code-signing.md).)

## Task 5: Docs + version

- [x] README: replace the unsigned/`xattr` notes with the signed-DMG story + local signing usage.
- [x] Spec M6c section: mark "now built (M6c)".
- [ ] `VERSION` bump for the first signed release (confirm value with maintainer; annotated tag `vX.Y.Z`).

---

## Secrets, CI wiring & maintainer setup

Signing material lives in **Doppler** (keys: `CODESIGN_P12_BASE64`, `CODESIGN_P12_PASSWORD`,
`CODESIGN_IDENTITY`, `KEYCHAIN_PASSWORD`, plus the API-key or Apple-ID notary set) and is fetched by
`release.yml` over **GitHub OIDC** (`dopplerhq/secrets-fetch-action`, `auth-method: oidc`) — GitHub stores
only the non-secret Variables `DOPPLER_IDENTITY_ID` / `DOPPLER_PROJECT` / `DOPPLER_CONFIG`, no secrets.
The full flow, how to **generate the Apple signing material**, the Doppler + OIDC setup, and **local
signing/notarization** all live in [`docs/code-signing.md`](../../code-signing.md).

## Verification gate

Agent smoke-test (no cert): ad-hoc sign a bundle → `codesign --verify --deep --strict` passes; `hdiutil`
DMG builds; `actionlint` clean; `cargo test --workspace` + `swift test` green. End-to-end notarized DMG +
CI signed release are maintainer steps (real Developer ID required).
