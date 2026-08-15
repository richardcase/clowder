# Keeping getclowder.app current: folding the site into this repo

*2026-08-15*

## The problem

The marketing site (`defiantsoftware/clowder-site`, public, Astro → GitHub Pages at
`getclowder.app`) keeps exactly one thing current on its own. `src/data/release.ts` reads the latest
release from the public Homebrew tap at build time — version, DMG URL, size — a daily `schedule:` in
its `deploy.yml` rebuilds, and CI refuses to publish when the lookup goes stale or the download
404s. That mechanism is well-built and survives this design untouched.

Nothing else about the site is connected to the product at all. Four kinds of drift go undetected:

1. **No public "what changed" channel exists.** This repo's release body carries real notes, but
   `scripts/update-homebrew-tap.sh:34` creates the public tap release with `--notes "Clowder
   $VERSION"` — a stub. The site has no changelog page. 0.6.0 shipped remote hosts, agent profiles,
   clipboard support and login-env capture; a visitor cannot discover that any of it exists.
2. **Feature and FAQ copy rots silently.** `Features.astro`, `Faq.astro` and `Architecture.astro` are
   hand-written prose with no link to what actually shipped.
3. **Hardcoded facts rot silently.** `site.ts` pins `minMacOS: '14'` — its comment says "Matches
   LSMinimumSystemVersion in the shipped 0.6.0 Info.plist" — and `arch: 'Apple silicon'`, "Verified
   with `lipo -archs`". Both were checked by hand, once. The real value lives at
   `scripts/build-app.sh:58`, and nothing connects the two.
4. **The screenshots are 0.5.0 captures** while 0.6.0 ships. That fact is recorded only in README
   prose.

## Root cause, and why the topology changes

Copy drift is only *preventable* when the pull request shipping a feature and the copy describing it
sit in one review. Across two repositories they cannot, so every two-repo design compensates after
the fact — the strongest version being an agent that reads published release notes and proposes edits
to copy it must fetch from elsewhere. That detects drift. It does not prevent it, and it introduces
an LLM and an API key into the publishing path for a problem that is really about where files live.

Folding the site into this repository under `site/` makes "update the site copy" a line item in the
pull request that causes the drift, enforceable by the same CI that already enforces commit message
grammar.

It also collapses most of the plumbing the two-repo version needed:

| Concern | Two repos | One repo |
|---|---|---|
| Render release notes | Fetch the tap API, retry ladder, hand-written safe-markdown parser for an untrusted string, CI-fatal staleness handling | An Astro content collection over local files |
| Product facts | Emit a JSON asset at release, publish to the tap, fetch, fall back | Read a file in the tree |
| Copy drift | Agent-authored pull request + `ANTHROPIC_API_KEY` on a public repo | A required check |

Only `release.ts` still needs the network, because the DMG genuinely lives on the tap and not in the
tree.

**Confirmed before committing to this.** GitHub Pages is available in private repositories on
GitHub Team, and a Pages site's visibility is independent of its repository's — the repo stays
private, the site stays public. Access control on a *private* Pages site is the Enterprise Cloud
feature, and is not what we want.

## The design

Five milestones. Milestone 0 is the migration; 1–4 are the freshness mechanisms it enables. Each
gets its own plan and pull request per `AGENTS.md`, in order — Milestone 3 depends on Milestone 1's
fragment mechanism, and everything depends on 0.

### Milestone 0 — migrate the site into this repo

**Import as a plain copy, not a subtree.** `git subtree add` was the first choice and it is wrong
here. The import drags `clowder-site`'s fifteen non-merge commits into the pull request's lint range,
and four of them fail `scripts/check-commit-messages.sh` — `Initial commit`, `Bump esbuild and
astro`, `Add Astro marketing site for Clowder`, `Replace the UI illustration with real Clowder
screenshots`. That is the required `commit messages (conventional commits)` check, so the migration
PR could not merge. `--squash` does not rescue it either: it still leaves a non-merge `Squashed
'site/' content from …` commit in the range.

So the tree is copied in under one Conventional commit. The history stays in `clowder-site`, which
is archived rather than deleted precisely so it remains readable.

**Path-filtered CI that keeps the required check honest.** `main`'s ruleset requires exactly two
contexts: `build + test (macOS, unsigned)` and `commit messages (conventional commits)`. The first is
`runs-on: macos-15`, a 10× minute multiplier against Team's 3,000/month, so a site typo must not
trigger it. `clowder-site`'s own `ci.yml` warns against narrowing a required check's trigger and
prescribes pairing it with "an always-passing fallback job reporting the same check name". **That
advice is unsafe in this repo**, and it is worth being precise about why, because it is the single
easiest thing here to get wrong:

`scripts/check-runs-state.sh` treats `skipped` and `neutral` as non-success — deliberately, on the
principle that a check which did not execute has vouched for nothing — and it gates the release
workflow's bump-PR merge. An `if:`-skipped job **still files a check run**, with conclusion
`skipped`. So two jobs sharing `name: build + test (macOS, unsigned)` would file *two* check runs
under the required name, one `skipped` and one `success`; `classify()` selects the latest per name by
`max_by([started_at, id])`, so which one wins is a race, and losing it fails the release.

The safe shape is a gate job. The real work is renamed, and a cheap job that always runs carries the
required name and asserts on the real job's `result`:

```yaml
changes:              # did anything outside site/ change?
build-and-test-macos: # name: build + test (macOS, unsigned) — full   if: product changed
required-build-gate:  # name: build + test (macOS, unsigned)          if: always(), runs-on: ubuntu
```

One check run under the required name, deterministic, and `skipped` never appears under it. The
context *string* is unchanged, and `check-runs-state.sh` reads the required set live from the
ruleset, so no ruleset edit is needed.

The classification itself lives in `scripts/changed-scope.sh` with a `--self-test` rather than in
workflow YAML, for the reason that library of self-tested scripts exists: a rule that can only be
exercised by a real pull request is a rule that ships wrong.

`release/**` branches are treated as always-product, so the bump PR always gets the real build. It
touches `VERSION` and `Cargo.lock` so it would qualify anyway — but that is an accident of the
current file set rather than a guarantee, and the failure mode is a release merging on a no-op check.

`commit-lint` stays unfiltered: it is ubuntu, runs in seconds, and Milestone 1 adds a check to it. A
new `site-ci` job (`npm ci`, `npm run check`, `npm run build` → `audit.sh`, plus `audit-selftest.sh`)
runs on `site/**` changes and is deliberately *not* added to the ruleset — the two required contexts
stay as they are.

**Deploy.** `.github/workflows/deploy-site.yml`, ported from the site repo, triggered by `push` to
`main` filtered on `site/**`, the daily `schedule:`, and `workflow_dispatch` — never
`pull_request`. The site repo's comment that `pages: write` and `id-token: write` must be unreachable
from pull request code matters more here, not less, now that the same repository holds
`DOPPLER_TOKEN` and the signing path.

While we are here, close a staleness window that exists today: the tap release is published *after*
the bump PR merges, so the deploy triggered by that merge still sees the previous version, and the
site lags until the daily rebuild. Add `gh workflow run deploy-site.yml` as the last step of
`release.yml`'s `release` job, after `update-homebrew-tap.sh`. `workflow_dispatch` is the documented
exception to "refs pushed with `GITHUB_TOKEN` do not start workflow runs" — the same reason `ci.yml`
already accepts it.

**Do not** add a check that `VERSION` matches the tap's latest version. It becomes tempting once both
are local, and it would fail the deploy during exactly that normal window.

**Domain cutover.** GitHub allows one repository per custom domain, so this is a hard switch rather
than a parallel run: merge with Pages still served from `clowder-site`; dispatch `deploy-site.yml`
and confirm the artifact; remove the custom domain from `clowder-site` and disable its Pages; enable
Pages on this repo with source *GitHub Actions* and set `getclowder.app`; re-tick **Enforce HTTPS**
once the certificate re-provisions, which can take up to an hour and is the visible-downtime step.
`public/CNAME` moves with the source, so DNS is unchanged. Archive `clowder-site` rather than
deleting it — it keeps the public history and the pre-cutover state.

**Guard the new boundary.** `site/scripts/audit.sh` keeps both existing checks; the private-repo link
check is *more* relevant now, not less. Add a third guarding the new boundary. "Nothing outside
`site/` may appear in `dist/`" is the intent, but not literally checkable — `dist/` records no
provenance — so the tractable form is a leakage scan: product source extensions (`.rs`, `.swift`,
`.toml`, `.plist`, `.sh`, …) and marker strings that must never be published (`DOPPLER_TOKEN`,
`HOMEBREW_TAP_TOKEN`, `clowder_proto`, …).
The practical risk is low, since Astro bundles only what is imported, but the entire private tree now
sits beside the build. Fixtures go in `audit-selftest.sh`, per that file's own rule that a guard
whose failure mode is a silent pass is worse than no guard.

### Milestone 1 — a public release-notes channel

**Fragments.** Each user-facing change adds one file,
`site/src/content/releases/unreleased/<slug>.md` — one or two sentences aimed at a visitor. One file
per change, so parallel branches never conflict.

They live under `site/` rather than `docs/` deliberately: Astro's content collections read them
natively with no loader configuration pointed outside the project root. The tradeoff is a product
artifact filed under the site tree, which is worth not fighting Astro's root boundary for.

**A new `scripts/check-release-notes.sh`**, wired into the existing `commit-lint` job:

- a pull request whose commits include a `feat` requires an added fragment; `fix` does not, since
  most fixes are invisible to users, but may add one
- escape hatch: a `no-release-note` label, reported in the job summary so skipping is a visible choice
- **content guard** — reject `github.com/richardcase/clowder`,
  `github.com/defiantsoftware/clowder([/"?#]|$)`, `#\d+` pull request references, and internal
  milestone tokens (`\bm\d+[a-z]?\b`). This is the private→public boundary: this repo is private and
  the site is public, and today's release notes are full of PR links that 404 for every visitor and
  scope names like `m12b` that mean nothing to one.
- **`--self-test`**, run in the same job. A grep that matches nothing exits 0, so a green run is not
  otherwise evidence the guard ran — the same reasoning that already puts `next-version.sh
  --self-test` and `check-runs-state.sh --self-test` in this job.

It sources `scripts/lib/conventional.sh` rather than growing a second copy of the grammar, for the
reason that library exists.

**Collection at release time**, in `release.yml`'s `bump` job alongside `set-version.sh`, so it rides
the existing signed-commit pull request: concatenate `unreleased/*.md` sorted into
`site/src/content/releases/vX.Y.Z.md` with `version`/`date` frontmatter, delete the fragments, and
run the content guard on the result before committing. No fragments writes `Maintenance and fixes.`
rather than failing — a patch release of pure `fix` commits is legitimate and must not block.

`bump` is skipped when `VERSION` is already correct, which is the documented re-dispatch path in
`docs/versioning.md`, so the publish step reads the file from the tree rather than assuming this ran.

**Publication.** `scripts/update-homebrew-tap.sh` reads
`site/src/content/releases/v$VERSION.md`, strips frontmatter, hard-fails if it is absent, runs the
content guard, and passes `--notes-file` instead of `--notes "Clowder $VERSION"`. It must **also**
`gh release edit --notes-file` on the already-exists path: notes are currently set only at create
time, so a re-run silently keeps the stub, where the DMG upload already has this idempotency via
`--clobber`.

**Rendering.** `site/src/pages/whats-new.astro`, a content collection over
`src/content/releases/v*.md` sorted by version, plus a "New in X.Y.Z" band on the homepage and a nav
entry. Local files, so there is no API call, no retry ladder, no markdown parsing of an untrusted
string and no injection surface.

### Milestone 2 — facts read from the tree

A new `scripts/lib/product.sh` holding `MIN_MACOS=14.0` and `APP_ARCH=arm64`, sourced by
`build-app.sh` in place of the literal at line 58 — following the `scripts/lib/conventional.sh`
precedent of one definition shared by every consumer.

`site/src/data/site.ts` drops its `minMacOS` and `arch` constants and parses `product.sh` at build
time, **failing the build if the parse finds nothing**: a silent fallback here would recreate exactly
the failure mode the audit scripts exist to prevent. The `14 → Sonoma` display mapping stays in the
site and fails on an unmapped major, since that is the one thing the site knows that the build does
not.

`release.ts` is untouched.

### Milestone 3 — make copy drift a blocking check

This is what the merge bought, and it replaces the agent-authored pull request entirely.

`check-release-notes.sh` gains a second rule: when a pull request adds a fragment — that is, when it
ships something user-facing — it must **also** touch one of `site/src/components/Features.astro`,
`Faq.astro`, `Architecture.astro`, or `site/src/content/`, or carry a `no-site-copy` label, again
reported in the summary.

Deliberately coarse. It is a "did you think about this" gate, not a semantic one; it cannot tell good
copy from bad. What it changes is which outcome is the default: forgetting becomes the effortful
path, and the reviewer is the author, at the moment they still remember what the feature does.

`Features.astro` and `Faq.astro` both open with headers stating that every claim maps to something
that ships and that the limitations are stated deliberately. Both get quoted into `AGENTS.md`, so an
agent editing them inherits the rule.

### Milestone 4 — make screenshot staleness visible

Screenshots cannot be automated: they need a signed app on a Mac with real projects in it. So make
the lag checkable instead. `site.ts` gains `screenshotsVersion: '0.5.0'`, promoting a README prose
fact into a checkable one, and `audit.sh` gains a check that warns when it trails the released minor
and fails at two minors behind, with `audit-selftest.sh` fixtures for both.

Not a hard failure at one version behind: failing the deploy over a cosmetic lag takes the site down,
which contradicts the principle `release.ts` already articulates — a failed build leaves the previous
site up, and that is the safer of the two bad outcomes.

## Rejected alternatives

**Keep two repos and add an agent that opens copy-refresh pull requests.** This was the design before
the topology question was asked, and it works, but it detects drift instead of preventing it, needs a
fine-grained PAT or an `ANTHROPIC_API_KEY` on a public repository, and requires the release notes to
round-trip through the tap API purely so the site can read what this repo already knows. The monorepo
deletes all three.

**Auto-derive public release notes from commit subjects.** Free, and already computed by
`next-version.sh --notes`. Rejected: subjects read `feat(m12b): agent profile store`. Internal
milestone scopes and PR links are not visitor-facing, and sanitizing them into good copy is the
curation this avoids.

**Publish product facts as a JSON asset on the tap release.** Necessary in the two-repo design, and
pointless in this one — `LSMinimumSystemVersion` is a literal in a shell script in this tree.

**Path-filter the macOS job by skipping it, or by pairing it with a same-named fallback job.** Both
are the obvious implementations and both break releases, because an `if:`-skipped job still files a
`skipped` check run and `check-runs-state.sh` counts that as non-success by design. Hence the gate
job.

**`git subtree add` to preserve the site's history.** Four of its fifteen commit subjects fail the
required commit-message lint. See Milestone 0.

**Hard-fail the deploy on stale screenshots.** Takes the live site down over a cosmetic lag.

## Known caveats

- The cutover has real downtime while the TLS certificate re-provisions for the new repository, and
  it cannot be run in parallel with the old site because a custom domain maps to one repository.
- Site changes now bill Actions minutes against the org's private-repo allowance rather than being
  free on a public repo. The ubuntu jobs are cheap; the path filter is what keeps this true, and it
  is load-bearing rather than an optimization.
- Milestone 3's check cannot judge whether the copy is *correct* — only that it was touched. A
  determined author can satisfy it with whitespace. That is accepted: the gate is a prompt, not a
  proof.
- A pre-release run exercises fragment collection and the deploy dispatch but **not** the tap
  publication, because pre-releases deliberately skip `update-homebrew-tap.sh` entirely.

## Verification

**Milestone 0**, before cutting over:

- `npm ci && npm run check && npm run build` from `site/`; `audit.sh` passes against `site/dist`
- a pull request touching only `site/README.md` reports `build + test (macOS, unsigned)` as **success
  from the gate job**, consuming no macOS minutes, with exactly **one** check run under that name
- a pull request touching only `crates/` reports the same context from the real job
- `scripts/check-runs-state.sh --sha <site-only PR head>` classifies the no-op as success — this is
  literally what a release's merge gate will do
- `scripts/check-commit-messages.sh` passes over the migration branch, imported history included
- dispatch `deploy-site.yml` and confirm the artifact matches the live site *before* the cutover;
  afterwards, load `https://getclowder.app` and check the certificate and the download button

**Milestone 1:** `check-release-notes.sh --self-test`, then break fixtures — a `#42`, a
`richardcase/clowder` link, an `m12b` token — and confirm each fails. A scratch branch with a `feat`
and no fragment fails `commit-lint`; adding one passes; the `no-release-note` label passes and says
so. Run `VERSION=0.6.1 DMG=… TAP_REPO=<throwaway public repo> scripts/update-homebrew-tap.sh`
**twice** — the second run must still show the notes, which is the create-only bug being fixed.
`npm run dev` renders `/whats-new` and the homepage band.

**Milestone 2:** confirm the parsed values equal `defaults read …/Info.plist LSMinimumSystemVersion`
and `lipo -archs` against the current 0.6.0 build. Both must equal today's pinned `14` and Apple
silicon — a mismatch *is* the drift this catches. Corrupt `product.sh` and confirm the site build
fails rather than falling back.

**Milestone 3:** a pull request with a fragment and no copy change fails; adding a `Features.astro`
edit passes; the `no-site-copy` label passes and is reported.

**Milestone 4:** set `screenshotsVersion` to `0.5.0` (warn) then `0.4.0` (fail) against a released
0.6.0; `audit-selftest.sh` covers both.

**End to end:** per `docs/versioning.md`, the first dispatch after any `release.yml` change must be a
`prerelease` run.
