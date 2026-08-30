# Homebrew distribution (cask + tap)

Clowder ships a Homebrew **cask** from a **tap** (`richardcase/homebrew-clowder`). Each **final**
signed release auto-updates the cask, so users get new versions with `brew upgrade`.

> The signed DMG is (re)hosted on the **tap repo's** Releases rather than the cask pointing at the
> source repo's own Releases, and the cask points there. This predates clowder going public/open
> source (when the source repo's release assets weren't downloadable without auth) and hasn't been
> revisited since — now that the source repo is public too, pointing the cask directly at its
> Releases may be simpler; that's a candidate follow-up, not done here.

## Install

```sh
brew install --cask richardcase/clowder/clowder
# or:
brew tap richardcase/clowder
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

A public repo named `homebrew-<tap>` — here `richardcase/homebrew-clowder` (installed as
`richardcase/clowder`). The first final release seeds `Casks/clowder.rb`; no manual cask commit needed.

```sh
gh repo create richardcase/homebrew-clowder --public --add-readme
```

### b. Create a fine-grained PAT and store it in Doppler

`GITHUB_TOKEN` can't touch another repo, and the release job needs the GitHub **API** (to upload the DMG
asset), which an SSH deploy key can't do — so use a **fine-grained personal access token** scoped to *only*
the tap repo:

1. GitHub → **Settings → Developer settings → Fine-grained tokens → Generate new token**. Resource owner
   **richardcase**; **Only select repositories → `homebrew-clowder`**; Repository permissions →
   **Contents: Read and write**. (Metadata read is added automatically.)

   > Fine-grained PATs are bound to their **resource owner**, so a token stops working the moment the tap
   > moves to a different owner — it must be re-minted against whichever owner holds the repo now. (If
   > the tap ever moves to an organization, note that orgs must separately permit fine-grained tokens
   > under Organization settings → **Personal access tokens**, which has no personal-account equivalent —
   > without it the token is created but denied.)
2. Put the token in the Doppler **release config** as `HOMEBREW_TAP_TOKEN` (alongside the signing
   secrets — see [`code-signing.md`](code-signing.md)).

That's it — GitHub still holds only the `DOPPLER_TOKEN` secret; the tap PAT lives in Doppler. The token
does both the DMG release-upload and the cask git push.

> Fine-grained PATs expire in ≤1 year — set a calendar reminder to rotate it (regenerate → update Doppler).
> A **missing** token warns + skips the bump; an **expired** one (present but invalid) fails the bump step
> red — a visible signal to rotate.

### The grant does not survive an owner transfer

A fine-grained PAT is granted against a **specific resource owner and repository**. Moving the tap to a
different owner does not carry the grant with it: the token keeps authenticating, so it does not look
expired, but every write returns `403 Resource not accessible by personal access token`.

This has happened twice now, and the timeline is worth keeping because it is not obvious from the symptom:

```
2026-08-12 12:25Z   v0.6.0 published to the tap — last release under the old grant
2026-08-12 12:36Z   main repo moved to the defiantsoftware org
2026-08-12 12:53Z   tap repo moved
2026-08-16          v0.7.0 — first release needing the token since — 403
2026-08-30          clowder open-sourced under Apache-2.0; main repo and tap repo both moved from
                     the defiantsoftware org back to the richardcase account. The PAT was reissued
                     under the new resource owner as part of the same change.
```

Four days of apparently healthy repos in 2026-08, because nothing had asked the token to write in between.
**After any owner transfer or repo rename, reissue the PAT and update Doppler before the next release** —
do not wait for the release to tell you.

`release.yml` now checks `repos/<tap>` for `.permissions.push` **before** it signs or tags anything
(`Verify the Homebrew tap token can still write`), so a bad grant stops the run while it is still cheap. It
used to surface at the tap publish, which runs *after* the tag and the GitHub Release — stranding a
signed, notarized, published release with nothing installable and manual cleanup to do.

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
brew info --cask richardcase/clowder/clowder
brew livecheck --cask clowder          # should report the latest final version
```
