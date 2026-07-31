# clowder M6f — Homebrew cask + tap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

> **Amendment (2026-07-31): the clowder source repo is private.** Its release assets aren't publicly
> downloadable, so the original plan below (cask URL → clowder repo; auth via an SSH deploy key) fails with
> a 404 in `brew`. Superseded: the signed **DMG is re-hosted on the public tap repo's Releases** and the
> cask points there, and auth is a single **fine-grained PAT** `HOMEBREW_TAP_TOKEN` (tap repo,
> contents:write) — doing both the DMG upload (GitHub API) and the cask push — so the **deploy key is
> dropped**. Read every "deploy key / SSH-push / clowder-repo URL" below as "PAT / https / tap-repo URL".
> See [`docs/homebrew.md`](../../homebrew.md).

**Goal:** Ship Clowder via a Homebrew **cask** in a personal tap (`richardcase/homebrew-clowder`), and
auto-bump it on every **final** signed release. `brew install --cask richardcase/clowder/clowder` installs
the notarized `Clowder.app` and puts the `clowder` CLI on `PATH`.

**Architecture:** the cask is generated from a template versioned in this repo; the release job renders it
(version + DMG sha256) and SSH-pushes it to the tap. Nothing about signing/app code changes.

**Tech Stack:** bash, Homebrew cask, GitHub Actions, SSH deploy key. Spec:
`docs/superpowers/specs/2026-07-30-muxy-m6-packaging-design.md` (§M6f). User-facing + setup docs:
[`docs/homebrew.md`](../../homebrew.md).

## Global Constraints

- **Auth is an SSH write deploy key** scoped to the tap repo only (no expiry, no account-wide access);
  the private key lives in Doppler as `HOMEBREW_TAP_DEPLOY_KEY`. GitHub still holds only `DOPPLER_TOKEN`.
- **Only final releases bump the cask.** `-` in the tag (e.g. `v0.3.0-rc1`) ⇒ GitHub pre-release, no cask
  bump, excluded from "Latest"/livecheck. One rule (`-` = pre-release) drives all three.
- **Bump failure is fatal**; the only graceful skip is a missing deploy key (`exit 0` + `::warning::`).
- **`brew style` gates the cask on every PR** (`ci.yml`); no `brew audit --new` (homebrew-core rules
  don't apply to a personal tap).
- Cask: `depends_on macos: :sonoma` (min macOS 14); notarized ⇒ no quarantine stanza; `zap` →
  `~/.config/clowder` only.
- `set -euo pipefail`; bash 3.2-safe.

---

## Task 1: Cask template + render/push script

- [x] `scripts/homebrew/clowder.rb.tmpl` — cask with `@@VERSION@@`/`@@SHA256@@`, `app "Clowder.app"` +
  `binary "#{appdir}/Clowder.app/Contents/MacOS/clowder"`, `livecheck :github_latest`,
  `depends_on macos: :sonoma`, `zap trash: "~/.config/clowder"`.
- [x] `scripts/update-homebrew-tap.sh` — `shasum -a 256` the DMG → render → write deploy key to a
  `chmod 600` temp file + `GIT_SSH_COMMAND` (`StrictHostKeyChecking=accept-new`) → clone
  `git@github.com:$TAP_REPO.git` → write `Casks/clowder.rb` → commit (bot identity) → push `main`.
  Idempotent; fatal on real failure.

## Task 2: Workflows

- [x] `release.yml`: `prerelease: ${{ contains(github.ref_name, '-') }}` on both publish steps; a
  **cask-bump** step after the signed publish, gated `steps.signing.outputs.enabled == 'true' &&
  !contains(github.ref_name, '-')`, reading `HOMEBREW_TAP_DEPLOY_KEY` from `steps.doppler.outputs.*`
  (skip-with-warning if absent).
- [x] `ci.yml`: render the template with placeholder version + dummy sha → `brew style` (fails the PR on
  cask errors).

## Task 3: Docs

- [x] `docs/homebrew.md` (new) — install, how auto-bump works, one-time setup (tap repo, deploy key →
  Doppler, cut a release), verify.
- [x] `docs/code-signing.md` — add `HOMEBREW_TAP_DEPLOY_KEY` to the Doppler-keys reference.
- [x] `README.md` — Homebrew install section; drop Homebrew from "Not yet done"; `docs/` row + link.
- [x] `docs/versioning.md` — link `homebrew.md`. **M6 spec §M6f** — marked built.

## Task 4: Tap repo bootstrap

- [ ] `gh repo create richardcase/homebrew-clowder --public --add-readme` (execution, confirm). The first
  final signed release seeds `Casks/clowder.rb`; the maintainer adds the deploy key + Doppler key.

## Verification gate

Agent: render → `brew style` (no offenses) + `ruby -c`; `actionlint` clean on both workflows. Maintainer:
add the deploy key to Doppler, push a final `vX.Y.Z` tag → cask appears/updates in the tap →
`brew install --cask richardcase/clowder/clowder` works on a clean Mac; a `v*-rc1` tag is a pre-release and
does not touch the cask.
