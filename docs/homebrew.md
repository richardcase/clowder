# Homebrew distribution (cask + tap)

Clowder ships a Homebrew **cask** from an organization **tap** (`defiantsoftware/homebrew-clowder`). Each **final**
signed release auto-updates the cask, so users get new versions with `brew upgrade`.

> The clowder **source repo is private**, so its release assets aren't publicly downloadable. The signed
> DMG is therefore (re)hosted on the **public tap repo's** Releases, and the cask points there — the
> source stays private while the binary is publicly installable.

## Install

```sh
brew install --cask defiantsoftware/clowder/clowder
# or:
brew tap defiantsoftware/clowder
brew install --cask clowder
```

This installs `Clowder.app` and puts the bundled `clowder` CLI on `PATH`. The build is signed +
notarized, so **no Gatekeeper workaround is needed**.

> The PATH `clowder` command is the same binary the app bundles; it talks to the **running app's** daemon
> socket. Use it alongside Clowder.app (launch the app first) — it is not a standalone daemon.

## How the auto-bump works

On a **final** `vX.Y.Z` tag, `release.yml` builds + signs + notarizes the DMG, publishes the GitHub
Release (on the private repo, as the internal record), then runs
[`scripts/update-homebrew-tap.sh`](../scripts/update-homebrew-tap.sh):

1. computes the DMG's `sha256`,
2. **uploads the DMG to the public tap repo's Releases** (`gh release`, so `brew` can fetch it unauthenticated),
3. renders [`scripts/homebrew/clowder.rb.tmpl`](../scripts/homebrew/clowder.rb.tmpl) with the version + sha,
4. pushes the rendered `Casks/clowder.rb` to the tap repo.

Both steps use the same fine-grained PAT (`HOMEBREW_TAP_TOKEN`) over https. Pre-release tags (anything with
a `-`, e.g. `v0.3.0-rc1`) are published as GitHub **pre-releases** and do **not** touch the cask — so `brew`
only ever sees final versions. The step is a no-op (with a warning) if the token isn't configured yet, and
fails the run if a real push fails (so cask drift is visible).

## One-time setup

### a. Create the tap repo

A public repo named `homebrew-<tap>` — here `defiantsoftware/homebrew-clowder` (installed as
`defiantsoftware/clowder`). The first final release seeds `Casks/clowder.rb`; no manual cask commit needed.

```sh
gh repo create defiantsoftware/homebrew-clowder --public --add-readme
```

### b. Create a fine-grained PAT and store it in Doppler

`GITHUB_TOKEN` can't touch another repo, and the release job needs the GitHub **API** (to upload the DMG
asset), which an SSH deploy key can't do — so use a **fine-grained personal access token** scoped to *only*
the tap repo:

1. GitHub → **Settings → Developer settings → Fine-grained tokens → Generate new token**. Resource owner
   **defiantsoftware**; **Only select repositories → `homebrew-clowder`**; Repository permissions →
   **Contents: Read and write**. (Metadata read is added automatically.)

   > Fine-grained PATs are bound to their **resource owner**, so a token issued under a personal account
   > stops working the moment the tap moves to the org — it must be re-minted with `defiantsoftware` as
   > the owner. The org must also permit fine-grained tokens (Organization settings → **Personal access
   > tokens**), which has no personal-account equivalent; without it the token is created but denied.
2. Put the token in the Doppler **release config** as `HOMEBREW_TAP_TOKEN` (alongside the signing
   secrets — see [`code-signing.md`](code-signing.md)).

That's it — GitHub still holds only the `DOPPLER_TOKEN` secret; the tap PAT lives in Doppler. The token
does both the DMG release-upload and the cask git push.

> Fine-grained PATs expire in ≤1 year — set a calendar reminder to rotate it (regenerate → update Doppler).
> A **missing** token warns + skips the bump; an **expired** one (present but invalid) fails the bump step
> red — a visible signal to rotate. Either way the signed Release itself is already published.

### c. Cut a release

```sh
scripts/set-version.sh X.Y.Z
git commit -am "Release vX.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"   # annotated; final tag (no '-') → cask is bumped
git push && git push origin vX.Y.Z
```

## Verify

```sh
brew style Casks/clowder.rb            # cask lint (also enforced on every PR in ci.yml)
brew info --cask defiantsoftware/clowder/clowder
brew livecheck --cask clowder          # should report the latest final version
```
