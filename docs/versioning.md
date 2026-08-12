# Versioning & releases

The version lives in one place: the top-level `VERSION` file.

- **Rust crates** inherit it via `[workspace.package] version` in the root `Cargo.toml`
  (`version.workspace = true` in each crate).
- **The macOS app** (`Clowder.app` Info.plist `CFBundleShortVersionString`/`CFBundleVersion`) reads
  `VERSION` in `scripts/build-app.sh`. A pre-release suffix is trimmed there — those keys must be
  dot-separated integers — so `0.5.0-rc1` ships a plist version of `0.5.0`.

You do not bump `VERSION` by hand for a release. The release workflow computes it and opens the PR
that changes it.

## Cutting a release

**Actions → Release → Run workflow**, from `main`. That is the whole procedure.

| Input | Effect |
|---|---|
| `dry_run` | Compute the version, print the plan to the job summary, change nothing. |
| `version_override` | Use an explicit version instead of the computed one — the escape hatch for cutting `1.0.0`. |
| `prerelease` | Append an identifier (e.g. `rc1`). Publishes as a GitHub pre-release and skips the Homebrew cask bump. |

Start with `dry_run: true` if you want to see what it would do.

## How the version is computed

`scripts/next-version.sh` finds the last release tag and reads the Conventional Commit subjects
since it (`--no-merges` — every PR lands as a `Merge pull request #N …` commit, which carries no
type). Run it locally any time; it changes nothing.

```sh
scripts/next-version.sh              # what would the next release be?
scripts/next-version.sh --notes      # the changelog for that range
scripts/next-version.sh --self-test  # the bump rules, as unit tests
```

| Commit type | Bump |
|---|---|
| `feat` | minor |
| `fix`, `perf` | patch |
| `!` before the colon, or a `BREAKING CHANGE:` footer | major — **but see the 0.x rule** |
| `docs`, `test`, `refactor`, `ci`, `chore`, `build`, `style`, `revert` | none |

The largest bump across the range wins. If **nothing** is releasable — a range of only docs and
chores, or no commits at all — the workflow reports that and exits green, having done nothing.
Non-releasable commits still appear in the release notes when a release does happen.

### The 0.x rule

**While the major version is 0, a breaking change bumps the minor version**, not the major: `0.4.0`
+ `feat!` → `0.5.0`. Under SemVer anything may change at any time in `0.x`, and promoting the
project to `1.0.0` should be a deliberate decision rather than a side effect of a commit marker.

When you do want `1.0.0`, pass it as `version_override`. After that the normal rules apply and `!`
bumps the major version.

The job summary reports the *effective* bump and, when the 0.x rule folded it, what the commits
originally asked for.

## What the workflow actually does

```
plan     compute the version; guard rails; dry_run stops here
  ↓
bump     set-version.sh → signed commit → PR → start CI → wait → merge   (skipped if VERSION is already correct)
  ↓
release  build → sign → notarize → TAG → publish → Homebrew cask bump
```

Two constraints shape this, and both are worth knowing before changing it:

**The bump goes through a PR because it has to.** `main`'s ruleset requires a pull request, signed
commits and passing checks, and has **no bypass actors** — `GITHUB_TOKEN` cannot push to `main`. The
commit is created with the GraphQL `createCommitOnBranch` mutation specifically because GitHub signs
commits made that way; a plain `git commit` in CI is unsigned and the merge would be rejected. (The
REST git-data API does *not* sign for you — its `signature` field is caller-supplied.)

**CI on the bump branch is started explicitly.** Refs pushed with `GITHUB_TOKEN` do not start
workflow runs, so the PR's required checks would never report and it could never merge.
`workflow_dispatch` is the documented exception, which is why `ci.yml` accepts it and the release
workflow calls `gh workflow run ci.yml --ref release/vX.Y.Z`. The resulting check runs attach to the
PR head SHA, which is what the ruleset evaluates.

A consequence: each release builds macOS twice — once for the bump PR's required check, once to
produce the artifact. That is the cost of requiring a green `main`.

**Tagging happens last, after the build succeeds.** A tag is the *input* to the next version
computation, so a tag left behind by a failed build would make the next run measure from it and
silently skip a version that never shipped.

## When something goes wrong

**The build failed after the bump merged.** This is the designed-for case and needs no cleanup:
`VERSION` on `main` is already correct, so re-dispatching recomputes the same version, sees the bump
is unnecessary, skips that job entirely, and goes straight to build → tag → publish.

**The bump PR did not merge.** It is left open on purpose, with the reason in the job summary. Merge
it by hand and re-dispatch, or close it and delete the `release/vX.Y.Z` branch to start over. The
distinct causes the workflow reports:

- *a required check concluded non-success* — including `cancelled`, `skipped` and `neutral`, which
  GitHub's rulesets count as passing but a release deliberately does not: a check that did not
  execute has not vouched for anything.
- *CI did not start on the branch* (10-minute deadline) — the `Start CI` dispatch is what makes the
  required checks run; check that `ci.yml` still accepts `workflow_dispatch`.
- *the PR is still `blocked` after 10 minutes* — the status-check rollup is a separate,
  eventually-consistent projection from the check runs the workflow waits on, so it can lag behind
  them. The merge retries; if it never converges, merge by hand.

**A tag exists but no release was published.** Possible if the tag push succeeded and publishing then
failed. `gh release delete --cleanup-tag` does not work in that state — delete the ref directly:
`gh api -X DELETE repos/defiantsoftware/clowder/git/refs/tags/vX.Y.Z`.

**The workflow refuses to start.** The guard rails in `plan` fail loudly rather than doing something
surprising:

- the tag already exists → that version shipped; `gh release delete vX.Y.Z --cleanup-tag`, or pass
  `version_override`
- a release PR is already open → merge or close it first
- a `release/vX.Y.Z` branch is left over → delete it
- dispatched from a branch other than `main` → only `dry_run` is allowed off `main`

## Artifacts

| | |
|---|---|
| Signed | `Clowder-X.Y.Z-macos.dmg` (no `v` prefix) |
| Unsigned fallback | `Clowder-X.Y.Z-macos.zip` |
| Tag | `vX.Y.Z` — annotated, created by CI |

Signing and notarization, and their one-time setup (signing material fetched from Doppler with a
single read-only service token), are documented in [`code-signing.md`](code-signing.md). A final
release also auto-bumps the Homebrew cask — see [`homebrew.md`](homebrew.md). Pre-releases are kept
out of "Latest" and skip the cask bump. The unsigned zip is Gatekeeper-quarantined on download —
right-click → Open, or `xattr -dr com.apple.quarantine Clowder.app`.

## Tags

Release tags are created by CI; don't tag by hand. They are **annotated but not signed** — no
ruleset requires a signed tag, and putting the signing key into Actions would be a worse trade than
the tag being merely annotated. The commit a tag points at is GitHub-signed either way.

On a dev machine a bare `git tag vX.Y.Z` fails with "no tag message?" — that is `tag.gpgsign = true`
in the maintainer's global git config, **not** a repo hook. CI has no such config, which is exactly
why `release.yml` passes `-a` explicitly: without it, CI would silently create a lightweight tag.

## After changing `release.yml`

**The first dispatch after any change to `release.yml` must be a `prerelease` run.**

Most of this workflow cannot execute outside a real release — the `bump` job's whole difficulty comes
from `main`'s ruleset (signed commits, required checks, no bypass actors), so there is nowhere else
to exercise it. Two rounds of bugs have reached `main` for exactly that reason. A `prerelease: rc1`
run drives the entire path — signed commit, PR, CI dispatch, check wait, merge, build, sign,
notarize, tag, publish — and skips only the Homebrew cask bump.

Undo is `gh release delete vX.Y.Z-rc1 --cleanup-tag`. `next-version.sh` excludes pre-release tags
from `git describe`, so a later final release still measures from the last *final* tag.

The parts that *can* be tested without a release are covered by
`scripts/check-runs-state.sh --self-test` and `scripts/next-version.sh --self-test`, both wired into
the required `commit messages (conventional commits)` job. Put new logic there where possible —
`check-runs-state.sh --sha <sha>` also classifies any real commit locally.

## One-time repo setup

The workflow depends on two settings that are easy to lose and hard to diagnose:

```sh
# Actions must be allowed to open the bump PR, or `POST /pulls` returns 403.
gh api -X PUT repos/defiantsoftware/clowder/actions/permissions/workflow \
  -F default_workflow_permissions=read -F can_approve_pull_request_reviews=true

# The `release` label marks the bump PR so it is excluded from its own release notes
# (see .github/release.yml) and so the guard rail can find an already-open one.
gh label create release -c 0E8A16 -d "Automated release version bump"
```
