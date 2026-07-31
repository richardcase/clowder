# Code signing, notarization & release

How `Clowder.app` is signed with a Developer ID, notarized by Apple, packaged as a DMG, and released —
plus the one-time setup to enable it. Releases fetch all signing material from **Doppler**; GitHub stores
only a single read-only, revocable **Doppler service token** — **no Apple signing material lives in GitHub**.

## The pipeline

```
scripts/build-app.sh    assemble dist/Clowder.app  (app exe + clowder-daemon/clowder/clowder-hook in
                        Contents/MacOS/, Info.plist, icon) — UNSIGNED
scripts/sign-app.sh     codesign each executable inner-first with the Developer ID, hardened runtime
                        (--options runtime), secure timestamp; then the .app bundle
scripts/package-dmg.sh  hdiutil DMG (drag-to-Applications layout) → sign → notarytool submit --wait →
                        stapler staple → verify (stapler validate + spctl)
```

`sign-app.sh` and `package-dmg.sh` take all inputs from **environment variables** (below), so the same
scripts run locally and in CI — only how the env is populated differs.

| Env var | Used by | Meaning |
|---|---|---|
| `CODESIGN_IDENTITY` | sign-app, package-dmg | `Developer ID Application: NAME (TEAMID)` (or `-` for an ad-hoc smoke test) |
| `CODESIGN_KEYCHAIN` | sign-app | optional keychain to search for the identity (CI temp keychain) |
| `NOTARY_PROFILE` | package-dmg | `notarytool` keychain profile (local convenience) |
| `NOTARY_KEY` / `NOTARY_KEY_ID` / `NOTARY_ISSUER` | package-dmg | App Store Connect API key path + IDs |
| `NOTARY_APPLE_ID` / `NOTARY_PASSWORD` / `NOTARY_TEAM_ID` | package-dmg | Apple ID + app-specific password |

`package-dmg.sh` auto-selects the first fully-present notary credential set (`NOTARY_PROFILE` › API key ›
Apple ID); with none set it builds + signs the DMG but skips notarize/staple.

## CI: Doppler via a service token

`.github/workflows/release.yml` (on a `v*` tag) builds the app, then — **only when the `DOPPLER_TOKEN`
secret is set** — fetches the signing material from Doppler, signs, and notarizes:

```
GitHub Actions
  └─ dopplerhq/secrets-fetch-action authenticates with a read-only Doppler SERVICE TOKEN
     (secrets.DOPPLER_TOKEN — scoped to one project+config)
       └─ fetches the signing secrets as auto-masked step outputs
            └─ mapped into env → sign-app.sh + package-dmg.sh → notarized DMG on the Release
```

- **Only one credential in GitHub:** the `DOPPLER_TOKEN` **secret** — a read-only, config-scoped,
  revocable Doppler service token. No Apple signing material is stored in GitHub; it all lives in Doppler.
  (Project/config are baked into the token's scope, so no other GitHub config is needed.)
- **The gate.** `DOPPLER_TOKEN` being present selects the signed path; a fork / a repo without it falls
  back to publishing an **unsigned `.zip`**.
- Fetched secret values are masked in the Actions log by the fetch action.

> **Why a service token, not OIDC?** OIDC (no stored token) needs a Doppler **Service Account Identity**,
> which is a **Team/Enterprise** feature. On the free Developer plan, a service token is the way. If you
> upgrade to a Team plan, `secrets-fetch-action` also supports `auth-method: oidc` (identity + `id-token:
> write`, no stored token) — see the action's docs.

## One-time setup

### a. Generate the Apple signing material

Requires **Account Holder / Admin** on the Apple Developer team.

**Developer ID Application certificate + private key** → `CODESIGN_IDENTITY`, `CODESIGN_P12_BASE64`,
`CODESIGN_P12_PASSWORD`:

1. Create the certificate — easiest via Xcode: **Xcode → Settings → Accounts → (your Apple ID) → Manage
   Certificates… → + → "Developer ID Application"**. This installs the cert **and its private key** into
   your login keychain.
   _Alternative (portal):_ [developer.apple.com → Certificates](https://developer.apple.com/account/resources/certificates/list)
   → **+** → "Developer ID Application" → upload a CSR created in **Keychain Access → Certificate
   Assistant → Request a Certificate from a Certificate Authority** (save to disk) → download the `.cer`
   → double-click to install.
2. Read the identity string:
   ```sh
   security find-identity -v -p codesigning
   ```
   The `Developer ID Application: NAME (TEAMID)` line is **`CODESIGN_IDENTITY`**. The parenthesized
   `TEAMID` is your **Team ID**.
3. Export as `.p12` **with the private key**: Keychain Access → **login** keychain → **My Certificates**
   → expand the "Developer ID Application…" identity (so the key is included) → right-click → **Export…**
   → save as `.p12`. The password you set is **`CODESIGN_P12_PASSWORD`**.
4. Base64-encode for Doppler:
   ```sh
   base64 -i DeveloperID.p12        # → CODESIGN_P12_BASE64
   ```
   (Line wrapping is fine — CI decodes with `base64 --decode`, which ignores whitespace.)

**Notarization credential — pick ONE** (the API key is the default and is recommended for CI):

- **App Store Connect API key** → `NOTARY_KEY_BASE64`, `NOTARY_KEY_ID`, `NOTARY_ISSUER`:
  [App Store Connect](https://appstoreconnect.apple.com/access/integrations/api) → **Users and Access →
  Integrations → App Store Connect API → Team Keys** → generate a key with **Developer** access.
  > ⚠️ It must be a **Team** key with the **Developer** role — *personal* keys are not eligible for the
  > Notary API.

  Download `AuthKey_XXXXXXXXXX.p8` — this is a **one-time download**. Then:
  - **`NOTARY_KEY_ID`** = the 10-character Key ID.
  - **`NOTARY_ISSUER`** = the Issuer ID (UUID shown at the top of the Keys page).
  - **`NOTARY_KEY_BASE64`** = `base64 -i AuthKey_XXXXXXXXXX.p8`.

- **Apple ID + app-specific password (alternative)** → `NOTARY_APPLE_ID`, `NOTARY_PASSWORD`,
  `NOTARY_TEAM_ID`: create an app-specific password at
  [appleid.apple.com](https://account.apple.com/account/manage) → **Sign-In and Security → App-Specific
  Passwords → +**. `NOTARY_APPLE_ID` is your Apple ID email; `NOTARY_TEAM_ID` is the Team ID from step 2.

**Ephemeral keychain password** → `KEYCHAIN_PASSWORD`: any random string (CI creates a throwaway keychain
with it), e.g. `openssl rand -base64 24`.

### b. Put the material in Doppler

Create a Doppler **project** and **config** for releases, and add the secrets using the **exact names**
the scripts read:

- `CODESIGN_P12_BASE64`, `CODESIGN_P12_PASSWORD`, `CODESIGN_IDENTITY`, `KEYCHAIN_PASSWORD`
- plus your chosen notary set: `NOTARY_KEY_BASE64` + `NOTARY_KEY_ID` + `NOTARY_ISSUER`, **or**
  `NOTARY_APPLE_ID` + `NOTARY_PASSWORD` + `NOTARY_TEAM_ID`.
- optional: `HOMEBREW_TAP_DEPLOY_KEY` — the tap repo's write deploy key, if you publish the Homebrew cask.
  A final signed release also auto-bumps the cask; see [`homebrew.md`](homebrew.md).

### c. Create a Doppler service token

In the Doppler dashboard → your project → the **release config** → **Access → Service Tokens →
Generate** → give it **read-only** access. Copy the token (starts with `dp.st.`). It is scoped to that
one config, so nothing else about the project needs to be configured in GitHub.

(See Doppler's [Service Tokens](https://docs.doppler.com/docs/service-tokens) docs. OIDC — no stored
token — instead needs a Service Account, which is a Team/Enterprise feature; see the note in the CI
section above.)

### d. Set the GitHub secret

GitHub → **Settings → Secrets and variables → Actions → _Secrets_** (the **Secrets** tab) → **New
repository secret**:

| Secret | Value |
|---|---|
| `DOPPLER_TOKEN` | the read-only Doppler service token from (c) (also the signing gate) |

Until `DOPPLER_TOKEN` is set the release stays unsigned. It's the **only** credential GitHub holds — a
read-only, config-scoped, revocable token; no Apple signing material lives in GitHub.

### e. Cut a signed release

```sh
scripts/set-version.sh 0.2.0        # updates VERSION + Cargo.lock
git commit -am "Release v0.2.0"
git tag -a v0.2.0 -m "v0.2.0"       # annotated (plain `git tag` is rejected in this repo)
git push && git push origin v0.2.0
```

`release.yml` builds, fetches the signing material from Doppler with the service token, signs +
notarizes, and attaches a `Clowder-vX.Y.Z-macos.dmg` to the GitHub Release.

## Local signing & notarization

You can run the full flow on your own machine (needs the Developer ID cert in your login keychain and
notary credentials):

```sh
scripts/build-app.sh
CODESIGN_IDENTITY="Developer ID Application: NAME (TEAMID)" scripts/sign-app.sh

# store notary credentials once (API-key example), then reuse via NOTARY_PROFILE:
xcrun notarytool store-credentials clowder-notary \
  --key AuthKey_XXXXXXXXXX.p8 --key-id XXXXXXXXXX --issuer <issuer-uuid>

NOTARY_PROFILE=clowder-notary \
  CODESIGN_IDENTITY="Developer ID Application: NAME (TEAMID)" \
  scripts/package-dmg.sh
```

Expect `notarytool` to report **Accepted**, `stapler validate` to pass, and `spctl -a -t open` to accept
the DMG. Mount it on a clean Mac → `Clowder.app` launches with no Gatekeeper prompt.

For a credential-free structural smoke test (no Apple account), ad-hoc sign:
`CODESIGN_IDENTITY="-" scripts/sign-app.sh` → `codesign --verify --deep --strict dist/Clowder.app`.

## References

- Apple — [Signing Mac software with Developer ID](https://developer.apple.com/developer-id/),
  [Certificates overview](https://developer.apple.com/support/certificates/)
- Doppler — [Service Tokens](https://docs.doppler.com/docs/service-tokens),
  [secrets-fetch-action](https://github.com/DopplerHQ/secrets-fetch-action); OIDC upgrade path (Team plan):
  [GitHub OIDC examples](https://docs.doppler.com/docs/github-oidc-examples) /
  [Service Account Identities](https://docs.doppler.com/docs/service-account-identities)
