# Homebrew distribution (cask + tap)

Clowder ships a Homebrew **cask** from a personal **tap** (`richardcase/homebrew-clowder`). Each **final**
signed release auto-updates the cask, so users get new versions with `brew upgrade`.

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
Release, then runs [`scripts/update-homebrew-tap.sh`](../scripts/update-homebrew-tap.sh):

1. computes the DMG's `sha256`,
2. renders [`scripts/homebrew/clowder.rb.tmpl`](../scripts/homebrew/clowder.rb.tmpl) with the version + sha,
3. SSH-pushes the rendered `Casks/clowder.rb` to the tap repo (write deploy key).

Pre-release tags (anything with a `-`, e.g. `v0.3.0-rc1`) are published as GitHub **pre-releases** and do
**not** touch the cask — so `brew` only ever sees final versions. The step is a no-op (with a warning) if
the deploy key isn't configured yet, and fails the run if a real push fails (so cask drift is visible).

## One-time setup

### a. Create the tap repo

A public repo named `homebrew-<tap>` — here `richardcase/homebrew-clowder` (installed as
`richardcase/clowder`). The first final release seeds `Casks/clowder.rb`; no manual cask commit needed.

```sh
gh repo create richardcase/homebrew-clowder --public --add-readme
```

### b. Create a write deploy key and store it in Doppler

`GITHUB_TOKEN` can't push to another repo, so the release job authenticates with an SSH **deploy key**
scoped to *only* the tap repo (no expiry, no account-wide access):

```sh
ssh-keygen -t ed25519 -f clowder-tap-deploy -C "clowder tap deploy" -N ""
```

1. Add the **public** key (`clowder-tap-deploy.pub`) to the tap repo:
   **Settings → Deploy keys → Add deploy key**, and **check "Allow write access"**.
2. Put the **private** key (`clowder-tap-deploy`, the whole PEM including the header/footer lines) in the
   Doppler **release config** as `HOMEBREW_TAP_DEPLOY_KEY` (alongside the signing secrets — see
   [`code-signing.md`](code-signing.md)). Delete the local key files afterward.

That's it — GitHub still holds only the `DOPPLER_TOKEN` secret; the tap key lives in Doppler.

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
