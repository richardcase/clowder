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

**Domain cutover** *(completed 2026-08-15; recorded here as it actually happened, because the
original order caused an outage)*. GitHub allows one repository per custom domain, so this is a hard
switch rather than a parallel run: merge with Pages still served from `clowder-site`; dispatch
`deploy-site.yml` and confirm the artifact; remove the custom domain from `clowder-site` and disable
its Pages; enable Pages on this repo with source *GitHub Actions* and set `getclowder.app`.
`public/CNAME` moves with the source, so DNS is unchanged. Archive `clowder-site` rather than
deleting it — it keeps the public history and the pre-cutover state.

**Then dispatch `deploy-site.yml` immediately.** This is the step the original plan lacked, and its
absence took the site down. PR #94 merged at 14:26:58 and `deploy-site.yml` fired three seconds
later; `build` passed and `deploy` failed `404 — Ensure GitHub Pages has been enabled`, because Pages
was not yet enabled here. Pages and the domain were then moved across, but **nothing re-triggered the
deploy** — the merge-triggered run is spent and does not retry — so `getclowder.app` served 404 until
a manual dispatch at 14:46:57.

**There is no "re-tick Enforce HTTPS" step, and there cannot be.** `getclowder.app` is fronted by
**Cloudflare** (DNS resolves to `104.21.28.148` / `172.67.146.190`; responses carry `server:
cloudflare`). GitHub Pages cannot complete its ACME challenge for a proxied domain, so
`https_enforced` stays `false` by necessity rather than oversight — do not go looking for the toggle.
TLS termination, the HTTP→HTTPS redirect (**Always Use HTTPS**) and the encryption mode
(**Full**/**Full (strict)** — never Flexible, which leaves the Cloudflare→Pages leg in plain HTTP)
are all configured in Cloudflare, not in GitHub.

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

**Audience.** `/whats-new` is written for an existing user deciding whether to upgrade, and doubles
as a liveness signal — at six releases in twelve days, a dated feed is itself evidence the project
ships. A fragment therefore states **one user-facing capability in plain language**: "connect the app
to a Clowder daemon on another machine over TLS", not "added `clowder remote
add|list|show|set|rm|probe|trust|untrust`". It is not a complete change record, and it is not
marketing copy — the Features section is that, and Milestone 3 is what keeps it honest.

**Fragments.** Each user-facing change adds one file, `site/src/content/unreleased/<slug>.md` — one
or two sentences. One file per change, so parallel branches never conflict.

They are a **sibling** of `site/src/content/releases/`, never nested inside it. Nesting works only
while the collection's glob pattern stays `*.md`; a later change to `**/*.md` would silently render
in-flight notes as shipped releases, and the sibling layout costs nothing to avoid that.

They live under `site/` rather than `docs/` because **everything that consumes them is site-scoped**:
`deploy-site.yml` filters on `paths: [site/**]`, so notes under `docs/` would change the site's
content without triggering a deploy; `scripts/changed-scope.sh` classifies `docs/` as `product`, so a
notes-only change would burn a full macOS build; and the renderer lives there. (An earlier draft
claimed Astro could not load from outside its root. That is false — the `glob` loader's `base` is
"relative to the root directory, or an absolute file URL", and its own error message says *"Glob
patterns cannot start with `../`. Set the `base` option to a parent directory instead."* Astro was
never the constraint.) The price is `update-homebrew-tap.sh`, a release script, reading a path inside
the marketing site.

**A new `scripts/check-release-notes.sh`**, wired into the existing `commit-lint` job:

- a pull request whose commits include a `feat` **or** a `fix` requires an added fragment — **per pull
  request, not per commit**, so three fixes need one note. Measured against real history, this is not
  the burden it sounds: of ~157 `feat`/`fix` commits since v0.3.0, only seven are non-user-facing
  (`fix(ci)` ×4, `fix(site)`, `fix(review)`, `fix(release)`).
- escape hatch: a `no-release-note` label, reported in the job summary so skipping is a visible
  choice. Labelling does **not** re-trigger CI — `labeled` is not among the default `pull_request`
  activity types, and adding it would re-run the macOS build on every toggle — so the flow is: label,
  then **Re-run failed jobs**, which re-runs `commit-lint` alone since nothing depends on it. The
  label needs creating once, like the `release` label (see the one-time setup in `versioning.md`).
- the failure message must say outright that an internal-only fix should take the label rather than
  invent filler. Requiring a note on every `fix` creates pressure to write noise; the guidance belongs
  where it is hit.
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
the existing signed-commit pull request. This is harder than it looks: the bump commit is **not** a
`git commit`. It is a GraphQL `createCommitOnBranch` mutation — used precisely because GitHub signs
commits made that way and `main`'s ruleset requires signatures — and it carries a **hardcoded list of
three additions with no `deletions` field at all**. Collecting fragments needs one addition at a
variable path and N deletions.

So the mutation's file list is generalized rather than special-cased, via two scripts that keep the
untestable surface down to wiring:

- **`scripts/collect-release-notes.sh <version>`** concatenates `site/src/content/unreleased/*.md`
  sorted into `site/src/content/releases/vX.Y.Z.md` with `version`/`date` frontmatter, deletes the
  fragments and runs the content guard. No fragments writes `Maintenance and fixes.` rather than
  failing — a patch release of pure `fix` commits is legitimate and must not block.
- **`scripts/gh-file-changes.sh`** turns the working tree's changes against `HEAD` into the mutation's
  `fileChanges` object. Both carry `--self-test`; both are pure functions, the shape
  `check-runs-state.sh` already proves can be pinned with fixtures.

Generalizing is also strictly more robust than today: the hardcoded list is hand-maintained, so
anything new `set-version.sh` ever touched would be silently dropped from the bump commit.

`bump` is skipped when `VERSION` is already correct, which is the documented re-dispatch path in
`docs/versioning.md`, so the publish step reads the file from the tree rather than assuming this ran.

**Publication — enforce early, degrade late.** `scripts/update-homebrew-tap.sh` runs at **step 17 of
the `release` job, after step 14 tags the commit and step 15 publishes the GitHub Release.** So it
must never abort the run: if the notes file is absent it **warns and falls back to the stub body**.
Hard-failing there would strand a signed, notarized, tagged, published release with no installable
artifact on the tap — exactly the broken state this document's own "when something goes wrong" advice
exists to recover from — over a missing markdown file. Enforcement belongs in the `bump` job instead,
where failing costs nothing because nothing has been published.

Otherwise it strips frontmatter, runs the content guard, and passes `--notes-file` instead of
`--notes "Clowder $VERSION"`. It must **also** `gh release edit --notes-file` on the already-exists
path: notes are currently set only at create time, so a re-run silently keeps the stub, where the DMG
upload already has this idempotency via `--clobber`.

**Rendering.** `site/src/pages/whats-new.astro`, a content collection over
`src/content/releases/*.md` sorted by **parsed semver** (string order puts `0.9.0` above `0.10.0`),
plus a "New in X.Y.Z" band on the homepage and a nav entry. Local files, so there is no API call, no
retry ladder, no markdown parsing of an untrusted string and no injection surface.

The homepage band carries **no date**. The same mechanism that signals a healthy cadence signals
neglect the moment releases pause, automatically and with nobody deciding to do it; a version and a
couple of headline items degrade to merely uninformative instead. Dates stay on `/whats-new`, where a
changelog without them would be useless and the reader has the context to interpret them.

**The Nav must be fixed first.** It links `#features`, `#architecture`, `#faq`, `#top`; the site is a
single page today, so a real `/whats-new` route makes every one of those dead from that page. They
become root-relative, and the homepage's own in-page scrolling is the regression to re-test.

**Backfill.** 0.1.0, 0.2.0 and 0.3.0 all shipped on 2026-07-31, 0.3.0's release body has no bullets
at all, and 0.1.0's is a 39-bullet dump of the entire initial codebase — three same-day entries would
read as noise on a page whose job is signalling rhythm. They collapse into one "Initial release"
entry; 0.4.0, 0.5.0 and 0.6.0 get their own, rewritten from their release bodies. Those bodies are
PR-title dumps full of `richardcase/clowder` links and `m11a`-style scopes, so every backfilled file
goes through the content guard before it is committed — the backfill is the guard's hardest real
test.

### Milestone 2 — facts read from the tree *(DROPPED 2026-08-16 — evidence below)*

**Not built, deliberately.** This milestone existed to stop `minMacOS` and `arch` "silently rotting".
Measured before starting it, they never have:

- `LSMinimumSystemVersion` has changed **once in the project's history** — in the commit that first
  created `build-app.sh`, when the product was still called Muxy. `site.ts`'s `minMacOS`/`arch` have
  changed once, in the import commit.
- Both were verified **correct** against the actually shipped 0.7.0 artifact: mounting the DMG gives
  `LSMinimumSystemVersion 14.0` and `lipo -archs arm64`, matching `minMacOS: '14'` and
  `arch: 'Apple silicon'`.
- Over the same period, four *prose* corrections were needed. Nothing that this milestone guards
  drifted; everything that drifted was outside its scope. Milestone 3 targets that instead.

It also could not have worked as written for `arch`: `build-app.sh` does a plain
`cp target/release/$bin` with no `--target`, so "Apple silicon" is an emergent property of which
runner CI happens to use, not a fact declared anywhere in the tree. The milestone would have had to
*invent* the declaration rather than read it.

The one real defect in this family was found by a live failure rather than by drift:
`update-homebrew-tap.sh` accepts any `VERSION` without validation, while `set-version.sh` enforces
`X.Y.Z[-prerelease]`. Run by hand with `VERSION=v0.7.0`, it produced a `vv0.7.0` tap tag, a cask
pointing at a non-existent asset, and a broken `brew install`. Worth fixing on its own merits; it is
not what this milestone was about.

The original design, for the record:

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

**The original design for this milestone was vacuous, and is replaced.** It required a pull request
adding a fragment to "also touch one of `Features.astro`, `Faq.astro`, `Architecture.astro`, or
`site/src/content/`" — but fragments live in `site/src/content/unreleased/`, so adding the fragment
*is* touching `site/src/content/`. The condition and its satisfaction were the same act: every pull
request would have passed automatically, reporting green while checking nothing. That is exactly the
failure mode `site/scripts/audit-selftest.sh`'s header exists to prevent, and it is recorded here
rather than quietly deleted because the shape of the mistake — a gate whose trigger also satisfies it
— is easy to reintroduce.

**What replaced it follows the evidence.** Four corrections were needed during Milestones 0 and 1,
every one prose, every one caught by a reviewer rather than a mechanism. `Features.astro`'s *positive*
claims needed none: a positive claim stays true as a product grows. Both FAQ corrections were
**limitation** claims, which hold only until someone fixes the thing — and the person fixing it is
deep in Rust, not reading marketing copy. Drift here is not diffuse; it is concentrated in the
honest-limitations copy, which is the most valuable copy on the site and the least revisited.

**The falsifier already existed.** This repo's issues are written in almost the FAQ's own words —
#56 "M8 — Linux support" against "Not yet … Clowder is macOS only", #55 "M9c — PTY-host true
zero-disruption agent survival" against "the underlying process does not survive that", and #87
"Terminal grid does not reflow when the window is resized" against the reflow claim that went stale.
#87 closing *is* the event that should have flagged the FAQ.

So `scripts/check-copy-claims.sh`, wired into `commit-lint` beside `check-release-notes.sh`, with two
checks. Both fail the pull request, with a `no-copy-review` label as the deliberate escape — the same
shape as the release-note rule, so there is one mechanism to learn rather than two.

1. **A limitation whose issue has closed.** Each limitation entry in `Faq.astro` carries a `gap:`
   naming the issue that tracks it; the check fails on any that is closed. Entries asserting no
   limitation need no `gap`. This catches *binary* gaps precisely.
2. **A release note that contradicts an open limitation.** Check 1 cannot see a *partial* change: the
   daemon-restart drift happened while #55 was, and remains, legitimately open — what changed is that
   partial recovery landed while the FAQ described total loss. So for each open `gap:` issue, the
   significant words of its title are matched against fragments added in the pull request, and a hit
   fails with both texts side by side. Deliberately keyword-based rather than semantic: it is a prompt
   for a human, the label is the answer when the prompt is wrong, and the threshold is tuned against
   the real corpus rather than guessed.

The check calls the issues API, so it **retries twice and then fails** rather than passing silently —
the same reasoning that makes `check-runs-state.sh` treat a check that did not execute as failed. Its
message names an API error as a possible cause, so nobody reads a blip as a real contradiction.

Deliberately **not** in scope: any gate on `Features.astro`, whose positive claims have never drifted
— a rule forcing every feature pull request to edit marketing copy is friction with no evidence
behind it. And no LLM comparison: keyword overlap against a curated set of tracked gaps covers every
failure that has actually occurred, and it is testable offline.

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

- The cutover cannot be run in parallel with the old site, because a custom domain maps to one
  repository. In the event the downtime came not from TLS but from the deploy: the merge-triggered
  run failed before Pages existed here and nothing retried it, so the site was 404 for ~20 minutes
  until a manual dispatch. TLS was never a factor — Cloudflare terminates it (see the cutover).
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
