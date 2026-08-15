# Site Monorepo Migration (Milestone 0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the `clowder-site` marketing site into this repository under `site/`, with CI that
never spends macOS minutes on a site-only change and never lies about the required check.

**Architecture:** The site tree is copied in as a plain directory (not a subtree import — see Task 1)
and built by its own ubuntu-only CI job. The expensive `macos-15` job becomes conditional on a
scope classifier, and a cheap always-running **gate job** carries the required check name so exactly
one check run reports it. Deploying moves to `deploy-site.yml`, which the release workflow dispatches
after publishing to the tap.

**Tech Stack:** Astro 7 / Node 24 (`site/`), GitHub Actions, bash (`scripts/`), `withastro/action@v6`,
`actions/deploy-pages@v5`.

**Spec:** `docs/superpowers/specs/2026-08-15-clowder-site-monorepo-freshness-design.md`

## Global Constraints

- **This repo is private; the site is public.** Never introduce a link to
  `github.com/defiantsoftware/clowder` in `site/` — `site/scripts/audit.sh` check 1 fails the build
  on it. Public-facing links go to the tap, `https://github.com/defiantsoftware/homebrew-clowder`.
- **Never hardcode a `/clowder-site/` path prefix.** The site serves from the root of
  `getclowder.app`. Use the `asset()` helper in `site/src/data/site.ts`.
- **Commit messages are Conventional Commits** — `type(scope): subject`, type one of `feat`, `fix`,
  `docs`, `test`, `refactor`, `perf`, `ci`, `chore`, `build`, `style`, `revert`. Run
  `scripts/check-commit-messages.sh` before pushing. The type drives the released version, so a
  wrong type mis-versions the next release.
- **Prefix every cargo command** with `source "$HOME/.cargo/env" && `.
- **The required check context name is exactly `build + test (macOS, unsigned)`** and must not
  change. `scripts/check-runs-state.sh` reads the required set live from the branch ruleset, so the
  string is the contract.
- **`scripts/check-runs-state.sh` treats `skipped` and `neutral` as FAILED**, deliberately and more
  strictly than GitHub. Any design where the required name can report `skipped` breaks releases.
- **This milestone stops before the domain cutover.** `clowder-site` keeps serving
  `getclowder.app` throughout. Do not disable its Pages, do not touch DNS, do not archive it.
- Work on a feature branch. Do not commit to `main`.

---

### Task 1: Import the site tree under `site/`

**Do not use `git subtree add`.** It was the spec's first choice and it is wrong here: the import
drags `clowder-site`'s 15 non-merge commits into the PR's lint range, and **4 of them fail**
`scripts/check-commit-messages.sh` — verified:

```
Add Astro marketing site for Clowder                       FAIL
Bump esbuild and astro                                     FAIL
Initial commit                                             FAIL
Replace the UI illustration with real Clowder screenshots  FAIL
```

`--squash` does not help: it still leaves a non-merge `Squashed 'site/' content from …` commit in the
range. History is preserved in the `clowder-site` repo, which stays and gets archived later.

**Files:**
- Create: `site/**` (the imported tree)
- Delete after import: `site/.github/`

**Interfaces:**
- Produces: the `site/` directory with `site/package.json` (scripts `dev`, `build`, `check`, `test`),
  `site/.nvmrc` (contents: `24`), `site/scripts/audit.sh`, `site/scripts/audit-selftest.sh`,
  `site/astro.config.mjs`, `site/src/data/site.ts`, `site/src/data/release.ts`. Later tasks reference
  all of these paths.

- [ ] **Step 1: Clone the site at the commit being imported and record it**

```bash
cd "$(git rev-parse --show-toplevel)"
git clone --depth 1 https://github.com/defiantsoftware/clowder-site.git /tmp/clowder-site-import
git -C /tmp/clowder-site-import rev-parse HEAD   # note this SHA for the commit message
```

- [ ] **Step 2: Copy the tree in, minus its git metadata and its workflows**

`site/.github/workflows/` would be inert (GitHub only reads `.github/workflows/` at the repo root),
but leaving two dead copies of the CI to drift is worse than deleting them. Tasks 3–5 port their
content.

```bash
rm -rf /tmp/clowder-site-import/.git
mkdir -p site
cp -R /tmp/clowder-site-import/. site/
rm -rf site/.github
ls site   # expect: LICENSE README.md astro.config.mjs package.json package-lock.json public scripts src tsconfig.json .nvmrc .gitignore
```

`site/LICENSE` (Apache-2.0) is kept deliberately: the site was published under it, the archived repo
remains, and keeping the file avoids implying a relicence. `site/.gitignore` already covers `dist/`,
`.astro/` and `node_modules/`, and git honours it per-directory — no root `.gitignore` change needed.

- [ ] **Step 3: Verify the site builds from its new location**

```bash
cd site && npm ci && npm run check && npm run build && cd ..
```

Expected: `astro check` reports 0 errors; the build ends with `audit: passed`. The build reaches the
network to resolve the release from the tap — if you are offline it warns and uses the pinned
fallback, which is fine locally (`IS_CI` is false).

- [ ] **Step 4: Verify the self-test still passes from the new location**

```bash
cd site && npm test && cd ..
```

Expected: `audit-selftest: 8 passed, 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add site
git commit -m "$(cat <<'EOF'
feat(site): import the marketing site from clowder-site

Copied rather than subtree-imported: the imported history contains four
non-Conventional commit subjects ("Initial commit", "Bump esbuild and
astro", and two others), which would fail the required
`commit messages (conventional commits)` check on this PR. The full
history stays in defiantsoftware/clowder-site, which keeps serving
getclowder.app until the cutover.

site/.github/ is dropped; GitHub only reads workflows from the repo
root, and the ported versions land in later commits.

Imported from clowder-site@<SHA>
EOF
)"
scripts/check-commit-messages.sh
```

Expected: `✓ N commit(s) match Conventional Commits`.

---

### Task 2: `scripts/changed-scope.sh` — classify a change range

A pure classifier, testable locally, so the CI decision is not something only a real pull request can
exercise. This mirrors `next-version.sh --self-test` and `check-runs-state.sh --self-test`, which
exist because release logic that cannot run outside a release shipped two bugs.

**Files:**
- Create: `scripts/changed-scope.sh`

**Interfaces:**
- Produces: `scripts/changed-scope.sh <base-ref> <head-ref> <branch>` prints exactly `product` or
  `site-only` on stdout. `scripts/changed-scope.sh --self-test` exits 0 when all cases pass. Task 3
  consumes the stdout value.

- [ ] **Step 1: Write the script with its self-test**

Both at once here, because the self-test *is* the test and the classifier is 20 lines; splitting them
would mean committing a file that cannot run.

```bash
#!/usr/bin/env bash
# Classify a change range as `product` (needs the full macOS build) or `site-only` (does not).
#
# The macOS job is runs-on: macos-15 — a 10x Actions-minute multiplier on a private repo — so a
# typo in site/ must not trigger it. That saving is only safe if the classification is trustworthy,
# hence --self-test: this is a pure function of a path list, and CI is a terrible place to discover
# it is wrong.
#
# The rule is an ALLOWLIST: only `site/**` is cheap, and everything else — including docs/ — is
# product. A docs-only change therefore still pays for a macOS build. That is deliberate: widening
# the allowlist widens the set of changes that can reach `main` without the real build having run,
# and docs-only pull requests are rare enough that the trade is not close.
#
# Usage: scripts/changed-scope.sh <base-ref> <head-ref> [branch]
#        scripts/changed-scope.sh --self-test
set -euo pipefail

# cs_classify <branch> < <newline-separated paths> -> `product` | `site-only`
cs_classify() {
  local branch="${1:-}" path any=0

  # A release branch always gets the real build. It happens to touch VERSION and Cargo.lock today,
  # so it would classify as product anyway — but that is a property of the current file set, not a
  # guarantee, and the failure mode is a release merging on a check that built nothing.
  case "$branch" in
    release/*) echo product; return 0 ;;
  esac

  while IFS= read -r path; do
    [ -n "$path" ] || continue
    any=1
    case "$path" in
      site/*) ;;
      *) echo product; return 0 ;;
    esac
  done

  # No files changed at all (an empty range, or a push to main comparing against itself). Fail safe
  # to the real build rather than waving through a range we could not read.
  if [ "$any" -eq 1 ]; then echo site-only; else echo product; fi
}

self_test() {
  local pass=0 fail=0
  check() {
    local want="$1" name="$2" branch="$3" paths="$4" got
    got="$(printf '%s' "$paths" | cs_classify "$branch")"
    if [ "$got" = "$want" ]; then
      echo "  ok    $name ($got)"
      pass=$((pass + 1))
    else
      echo "  FAIL  $name — wanted $want, got $got" >&2
      fail=$((fail + 1))
    fi
  }

  echo "changed-scope: verifying the classification"

  check product   'rust source'            feature 'crates/clowder-daemon/src/main.rs'
  check product   'swift source'           feature 'macos/Sources/ClowderCore/BackendPlan.swift'
  check product   'the workflows'          feature '.github/workflows/ci.yml'
  check product   'docs are not cheap'     feature 'docs/versioning.md'
  check product   'a root file'            feature 'VERSION'
  check site-only 'site markdown'          feature 'site/README.md'
  check site-only 'several site files'     feature 'site/src/pages/index.astro
site/package.json'
  check product   'site plus product'      feature 'site/README.md
crates/clowder-proto/src/lib.rs'

  # A path that merely starts with the string `site` is NOT under site/.
  check product   'sitemap at the root'    feature 'sitemap.xml'
  check product   'a sibling dir'          feature 'site-notes/x.md'

  # Fail-safe cases.
  check product   'empty range'            feature ''
  check product   'blank lines only'       feature '

'
  # Release branches never take the cheap path, whatever they touch.
  check product   'release branch'         release/v0.7.0 'site/README.md'

  echo "changed-scope: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit $? ;;
  -h | --help)
    echo "Usage: scripts/changed-scope.sh <base-ref> <head-ref> [branch]"
    echo "       scripts/changed-scope.sh --self-test"
    exit 0
    ;;
esac

[ "$#" -ge 2 ] || { echo "error: need <base-ref> <head-ref>" >&2; exit 2; }

BASE="$1"
HEAD_REF="$2"
BRANCH="${3:-}"

if ! merge_base="$(git merge-base "$BASE" "$HEAD_REF" 2>/dev/null)"; then
  # No common ancestor means we cannot tell what changed. Build everything.
  echo product
  exit 0
fi

git diff --name-only "$merge_base" "$HEAD_REF" | cs_classify "$BRANCH"
```

- [ ] **Step 2: Run the self-test and watch it fail before the file is executable**

```bash
chmod +x scripts/changed-scope.sh
scripts/changed-scope.sh --self-test
```

Expected: `changed-scope: 13 passed, 0 failed`. If any case fails, fix the classifier — not the
expectation.

- [ ] **Step 3: Verify it against this actual branch**

```bash
scripts/changed-scope.sh origin/main HEAD "$(git branch --show-current)"
```

Expected: `product` — this branch has touched `docs/` and `scripts/`.

- [ ] **Step 4: Prove the cheap path is reachable, not just tested in fixtures**

```bash
git stash -u 2>/dev/null || true
tmp=$(git rev-parse --abbrev-ref HEAD)
git checkout -b tmp/scope-probe
echo "probe" >> site/README.md && git commit -qam "docs(site): scope probe"
scripts/changed-scope.sh origin/main HEAD tmp/scope-probe   # expect: site-only
git checkout "$tmp" && git branch -D tmp/scope-probe
git stash pop 2>/dev/null || true
```

Expected: `site-only`. This is the whole point of the milestone — confirm it against real git, not
only against the fixture list.

- [ ] **Step 5: Commit**

```bash
git add scripts/changed-scope.sh
git commit -m "ci: classify a change range as product or site-only

The macOS job is a 10x minute multiplier on a private repo, so a site
typo must not trigger it. Making the decision a pure function with a
--self-test follows next-version.sh and check-runs-state.sh: CI is a bad
place to find out a classifier is wrong."
```

---

### Task 3: Make the required macOS check conditional, behind a gate job

**The obvious implementation is a trap.** Two jobs sharing `name: build + test (macOS, unsigned)`
with opposite `if:` conditions produces **two check runs under the required name** — GitHub creates a
`skipped` check run for an `if:`-skipped job. `scripts/check-runs-state.sh` selects the latest per
name via `max_by([started_at, id])` and counts `skipped` as **failed**, so which one wins is a race,
and losing it fails the release merge gate.

Instead: rename the real job, and add a gate job that always runs and carries the required name.
The context string is unchanged, so **the branch ruleset needs no edit**.

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `scripts/changed-scope.sh <base> <head> <branch>` from Task 2.
- Produces: job id `changes` with output `scope`; job id `build-and-test-macos` (display name
  `build + test (macOS, unsigned) — full`); job id `required-build-gate` with
  `name: build + test (macOS, unsigned)`. Task 4 adds a job alongside these.

- [ ] **Step 1: Add the `changes` job**

Insert immediately after the `jobs:` line, before `commit-lint`:

```yaml
  # Decides whether this change needs the macOS build. Pure bash so the rule is unit-testable —
  # see scripts/changed-scope.sh --self-test, which runs in commit-lint below.
  changes:
    name: classify change scope
    runs-on: ubuntu-latest
    permissions:
      contents: read
    outputs:
      scope: ${{ steps.classify.outputs.scope }}
    steps:
      - uses: actions/checkout@v4
        with:
          # merge-base needs real history; the default depth-1 checkout has none.
          fetch-depth: 0

      - id: classify
        env:
          # Same reasoning as commit-lint: on `pull_request` actions/checkout checks out the merge
          # ref, so HEAD is not the branch tip. On a dispatch or push there is no PR context.
          BASE: ${{ github.event.pull_request.base.sha || 'origin/main' }}
          HEAD_REF: ${{ github.event.pull_request.head.sha || github.sha }}
          BRANCH: ${{ github.event.pull_request.head.ref || github.ref_name }}
        run: |
          scope="$(scripts/changed-scope.sh "$BASE" "$HEAD_REF" "$BRANCH")"
          echo "scope=$scope" >> "$GITHUB_OUTPUT"
          echo "change scope: **$scope**" >> "$GITHUB_STEP_SUMMARY"
```

On a push to `main`, `BASE` and `HEAD_REF` are the same commit, the diff is empty, and the classifier
fail-safes to `product`. That is the wanted behaviour, and it is covered by the `empty range` case.

- [ ] **Step 2: Add the self-test to the existing `commit-lint` job**

After the `check-runs-state.sh self-tests` step, matching the comment style already there:

```yaml
      # Same reasoning again: the scope classifier decides whether the macOS build runs at all, and
      # a wrong answer either burns 10x-rate minutes or lets a change reach main unbuilt. Fixtures
      # make it a required check.
      - name: changed-scope.sh self-tests
        run: scripts/changed-scope.sh --self-test
```

- [ ] **Step 3: Rename the real macOS job and make it conditional**

Change the job id and its `name:`, and add `needs`/`if`. Leave every step in it untouched.

```yaml
  build-and-test-macos:
    name: build + test (macOS, unsigned) — full
    needs: changes
    if: needs.changes.outputs.scope == 'product'
    runs-on: macos-15
    steps:
      # ... unchanged ...
```

- [ ] **Step 4: Add the gate job that carries the required name**

Append at the end of the file:

```yaml
  # THE REQUIRED CHECK. It always runs, so the required context always reports exactly one check
  # run, and it reports success only if the real build succeeded or was legitimately not needed.
  #
  # Why a gate rather than two jobs sharing this name: an `if:`-skipped job still produces a check
  # run, with conclusion `skipped`. Two jobs with this name would therefore file TWO check runs
  # under it, and scripts/check-runs-state.sh picks the latest by (started_at, id) and counts
  # `skipped` as FAILED — so the release merge gate would fail on a coin flip.
  required-build-gate:
    name: build + test (macOS, unsigned)
    needs: [changes, build-and-test-macos]
    # Without always(), this job would itself be skipped whenever build-and-test-macos is skipped,
    # which recreates the exact problem it exists to solve.
    if: always()
    runs-on: ubuntu-latest
    steps:
      - name: Require the macOS build to have passed, or to have been genuinely unnecessary
        env:
          SCOPE: ${{ needs.changes.outputs.scope }}
          BUILD: ${{ needs.build-and-test-macos.result }}
        run: |
          echo "scope=$SCOPE build=$BUILD"

          if [ "$SCOPE" = 'site-only' ] && [ "$BUILD" = 'skipped' ]; then
            echo "site-only change — the macOS build was not required." >> "$GITHUB_STEP_SUMMARY"
            exit 0
          fi

          if [ "$SCOPE" = 'product' ] && [ "$BUILD" = 'success' ]; then
            echo "product change — the macOS build passed." >> "$GITHUB_STEP_SUMMARY"
            exit 0
          fi

          # Anything else is a state we did not design for: a cancelled build, a failed one, or a
          # scope/result pair that cannot legitimately occur. Refuse rather than guess — this check
          # is what a release merges on.
          echo "::error::refusing to pass: scope=$SCOPE build=$BUILD"
          exit 1
```

- [ ] **Step 5: Lint the workflow locally**

```bash
python3 -c "import sys,yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
grep -n "name: build + test (macOS, unsigned)$" .github/workflows/ci.yml
```

Expected: `yaml ok`, and **exactly one** line matching the required name exactly (the gate). The real
job now ends in `— full`, so it must not match.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: skip the macOS build for site-only changes

Puts the required 'build + test (macOS, unsigned)' context on an
always-running gate job instead of on the macOS job itself. An
if:-skipped job still files a check run with conclusion 'skipped', and
check-runs-state.sh selects the latest run per required name and counts
'skipped' as failed — so two jobs sharing the required name would fail
the release merge gate whenever the skipped run sorted last.

The context string is unchanged, so main's ruleset needs no edit."
```

- [ ] **Step 7: Verify on a real pull request — this cannot be checked locally**

Push the branch, open a draft PR, and confirm on a **site-only** commit:

```bash
gh pr checks --watch
gh api "repos/defiantsoftware/clowder/commits/$(git rev-parse HEAD)/check-runs" \
  --jq '.check_runs[] | select(.name == "build + test (macOS, unsigned)") | {name, conclusion, started_at}'
```

Expected: exactly **one** check run under that name, `conclusion: success`, and no macOS job in the
run. Then the real gate test:

```bash
scripts/check-runs-state.sh --sha "$(git rev-parse HEAD)"
```

Expected: `build + test (macOS, unsigned)` classified `passed`. This is literally what
`release.yml`'s merge gate runs — if this says anything else, do not proceed to Task 4.

---

### Task 4: Add the `site-ci` job

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: job `changes` and its `scope` output from Task 3; `site/package.json` and `site/.nvmrc`
  from Task 1.
- Produces: job id `site-ci`. Deliberately **not** added to the ruleset's required set — the two
  required contexts stay as they are.

- [ ] **Step 1: Add the job**

Append before `required-build-gate` (ordering is cosmetic; jobs run on their `needs`):

```yaml
  # The site's own checks, ported from clowder-site's ci.yml. Ubuntu-only and cheap, so it runs
  # whenever the site changes. NOT in main's required set: adding a third required context is a
  # ruleset change with release-gating consequences, and this does not need to be one.
  site-ci:
    name: build + check (site)
    needs: changes
    # Runs on a product change too: a product change can still touch site/, and this job is cheap.
    if: always() && needs.changes.result == 'success'
    runs-on: ubuntu-latest
    permissions:
      contents: read
    defaults:
      run:
        working-directory: site
    steps:
      - uses: actions/checkout@v4

      # Before the build, for the reason documented in site/scripts/audit-selftest.sh: audit.sh's
      # dangerous failure mode is a silent pass, so a green build is not evidence the guards ran
      # unless the self-test ran first. Pure bash, so it needs no dependencies.
      - name: Verify the build guards actually detect violations
        run: ./scripts/audit-selftest.sh

      - name: Set up Node
        uses: actions/setup-node@v4
        with:
          node-version-file: 'site/.nvmrc'
          cache: 'npm'
          cache-dependency-path: 'site/package-lock.json'

      - name: Install
        run: npm ci

      # Type errors do not fail `astro build` — Astro strips types rather than checking them — so
      # without this step nothing would ever catch one.
      - name: Type-check
        run: npm run check

      # `npm run build` chains scripts/audit.sh, so the private-repo and base-path guards run here.
      #
      # GITHUB_TOKEN authenticates the release lookup in src/data/release.ts. Unauthenticated
      # api.github.com is capped at 60 requests/hour per IP and runner IPs are shared, so without it
      # the build intermittently falls back to the pinned version.
      - name: Build
        run: npm run build
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

Note `node-version-file` and `cache-dependency-path` are **repo-root relative** and ignore
`defaults.run.working-directory`, which applies only to `run:` steps. Hence the `site/` prefixes on
those two, and none on the `run:` commands.

- [ ] **Step 2: Verify the YAML and the path handling**

```bash
python3 -c "import sys,yaml; d=yaml.safe_load(open('.github/workflows/ci.yml')); print(sorted(d['jobs']))"
test -f site/.nvmrc && test -f site/package-lock.json && echo "paths ok"
```

Expected: `['build-and-test-macos', 'changes', 'commit-lint', 'required-build-gate', 'site-ci']` and
`paths ok`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run the site's build and checks on pull requests

Ports clowder-site's ci.yml. Not added to main's required set: a third
required context is a ruleset change with release-gating consequences,
and this does not need to be one."
```

- [ ] **Step 4: Verify on the pull request**

```bash
gh pr checks
```

Expected: `build + check (site)` present and passing. Confirm it ran on the site-only commit from
Task 3.

---

### Task 5: Port the deploy workflow

**Files:**
- Create: `.github/workflows/deploy-site.yml`

**Interfaces:**
- Produces: a `workflow_dispatch`-able workflow named `deploy-site.yml`. Task 6 dispatches it by
  that exact filename.

- [ ] **Step 1: Create the workflow**

```yaml
name: Deploy site to GitHub Pages

# NOTE: until the domain cutover, defiantsoftware/clowder-site still serves getclowder.app. This
# workflow can be dispatched to verify the build, but Pages is not yet enabled on this repo, so the
# `deploy` job will fail until it is. That is expected and is the cutover's first step.
on:
  push:
    branches: [main]
    paths:
      - 'site/**'
      - '.github/workflows/deploy-site.yml'
  # The version and download URL are read from the public Homebrew tap at build time, so a daily
  # rebuild keeps the site current after a Clowder release without anyone editing this repo.
  schedule:
    - cron: '0 6 * * *'
  # release.yml dispatches this after publishing to the tap, so a release does not wait for the
  # daily run to be advertised.
  workflow_dispatch:

# Deliberately NOT `pull_request`. pages:write and id-token:write must never be reachable from pull
# request code — which matters more here than it did in clowder-site, because this repo also holds
# DOPPLER_TOKEN and the signing path.

# Allow only one concurrent deployment, but let a running one finish so a half-published site is
# never left behind.
concurrency:
  group: pages
  cancel-in-progress: false

permissions:
  contents: read
  pages: write
  id-token: write

jobs:
  build:
    runs-on: ubuntu-latest
    # Set at job level rather than on the withastro/action step: env propagation into a composite
    # action's own steps is not something to rely on.
    #
    # This authenticates the release lookup in site/src/data/release.ts. Unauthenticated
    # api.github.com allows 60 requests/hour per IP and runner IPs are shared, so without it the
    # daily rebuild can silently fall back to the pinned version.
    env:
      GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - name: Checkout
        uses: actions/checkout@v7

      # Runs before the build because audit.sh's dangerous failure mode is a silent pass: a pattern
      # that matches nothing still exits 0. Without this, a green deploy is not evidence the guards
      # ran. No npm needed — it is pure bash against throwaway fixtures.
      - name: Verify the build guards actually detect violations
        run: ./scripts/audit-selftest.sh
        working-directory: site

      - name: Build with Astro
        uses: withastro/action@v6
        with:
          # The Astro project is not at the repo root any more.
          path: site
          # withastro/action has no node-version-file input, so this duplicates site/.nvmrc rather
          # than reading it. CI builds on site/.nvmrc — keep the two in step, or pull requests get
          # validated on a different Node than deploys.
          node-version: 24

  deploy:
    needs: build
    runs-on: ubuntu-latest
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - name: Deploy to GitHub Pages
        id: deployment
        uses: actions/deploy-pages@v5
```

- [ ] **Step 2: Verify the YAML and that the `path` input is real**

```bash
python3 -c "import yaml; d=yaml.safe_load(open('.github/workflows/deploy-site.yml')); print(sorted(d['jobs']), d['permissions'])"
```

Expected: `['build', 'deploy']` and `{'contents': 'read', 'pages': 'write', 'id-token': 'write'}`.
`withastro/action@v6` accepts `path` (default `.`), `node-version` (default `24`) and `out-dir`
(default `dist`) — confirmed against its `action.yml`.

- [ ] **Step 3: Assert it can never run on a pull request**

```bash
python3 -c "
import yaml; d=yaml.safe_load(open('.github/workflows/deploy-site.yml'))
on=d[True] if True in d else d['on']
assert 'pull_request' not in on, 'pages:write must not be reachable from PR code'
print('triggers:', sorted(on))
"
```

Expected: `triggers: ['push', 'schedule', 'workflow_dispatch']`. (`on:` parses as the boolean `True`
in YAML 1.1, hence the lookup dance.)

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/deploy-site.yml
git commit -m "ci: add the site deploy workflow

Ported from clowder-site's deploy.yml, with path: site for the relocated
Astro root. Not wired to a live domain yet — clowder-site keeps serving
getclowder.app until the cutover."
```

---

### Task 6: Dispatch the deploy from the release workflow

Closes a real gap: the tap release is published *after* the bump PR merges, so the deploy triggered
by that merge still reads the previous version, and the site advertises the old release until the
next daily run.

**Files:**
- Modify: `.github/workflows/release.yml` (the `release` job, after the `Update Homebrew tap` step at
  approximately line 632)

**Interfaces:**
- Consumes: `deploy-site.yml` from Task 5.

- [ ] **Step 1: Confirm the anchor step and its guard are still what this plan expects**

```bash
grep -n -A2 'name: Update Homebrew tap' .github/workflows/release.yml
```

Expected — the new step reuses this guard verbatim:

```yaml
      - name: Update Homebrew tap (publish DMG + cask bump)
        if: ${{ steps.signing.outputs.enabled == 'true' && needs.plan.outputs.prerelease != 'true' }}
```

Both halves matter. A pre-release never touches the tap, and neither does an unsigned run — so in
both cases the tap release is unchanged and there is nothing new for the site to read.

- [ ] **Step 2: Add the dispatch step immediately after the tap update**

Insert between `Update Homebrew tap` and `Remove temp signing keychain`.

```yaml
      # The tap release is what the site reads, and it is only published by the step above — after
      # the bump PR merged and its own push-triggered deploy already ran against the previous
      # version. Without this the site advertises the old release until the next daily rebuild.
      #
      # `workflow_dispatch` is the documented exception to "refs pushed with GITHUB_TOKEN do not
      # start workflow runs" — the same reason ci.yml accepts it.
      #
      # Deliberately not a failure: the release itself has already succeeded and been published by
      # this point, and the daily schedule is the backstop. Failing here would report a broken
      # release that is not broken.
      - name: Refresh the site (it reads the tap release)
        if: ${{ steps.signing.outputs.enabled == 'true' && needs.plan.outputs.prerelease != 'true' }}
        continue-on-error: true
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: gh workflow run deploy-site.yml --ref main
```

- [ ] **Step 3: Verify the YAML still parses and the step landed in the right job**

```bash
python3 -c "
import yaml; d=yaml.safe_load(open('.github/workflows/release.yml'))
names=[s.get('name') for s in d['jobs']['release']['steps']]
i=[n for n in names if n and 'Homebrew' in n]
print('tap step:', i)
print('dispatch after it:', names[names.index(i[0])+1])
"
```

Expected: the dispatch step's name printed immediately after the Homebrew step.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: refresh the site after publishing to the tap

The site reads the version and DMG URL from the tap release, which is
published after the bump PR merged — so the deploy that merge triggered
already ran against the previous version. continue-on-error because the
release has succeeded by this point and the daily rebuild is the
backstop."
```

---

### Task 7: Guard the new private/public boundary in `audit.sh`

The whole private tree now sits beside the site build. Astro bundles only what is imported, so the
risk is low today — but Milestone 2 deliberately reads `scripts/lib/product.sh` from outside `site/`,
so the boundary genuinely gets crossed by design and wants a guard before it does.

`dist/` does not record where its bytes came from, so "nothing outside `site/`" is not literally
checkable. The tractable form is a leakage scan: source-file extensions that could only come from the
product tree, and marker strings that must never be published.

**Files:**
- Modify: `site/scripts/audit.sh`
- Modify: `site/scripts/audit-selftest.sh`

**Interfaces:**
- Consumes: the existing `check()` helper in `audit-selftest.sh`, which writes `$3` as `index.html`
  inside a fixture dir and asserts `audit.sh`'s exit status.
- Produces: a new `check_file()` helper for fixtures that need a specific *filename* rather than
  specific content.

- [ ] **Step 1: Add check 3 to `site/scripts/audit.sh`**

Insert before the final `if [[ $status -eq 0 ]]` block:

```bash
# --- 3. private-source leakage -----------------------------------------------
# The site now lives inside the private product repo, so a bad import or a build-time file read
# could publish something that was never meant to be public. dist/ carries no provenance, so this
# checks for what leaked source would look like rather than where it came from.
#
# Milestone 2 makes site.ts read scripts/lib/product.sh at build time — a deliberate read from
# outside site/. This guard exists so the next one is not silently broader.
leaked=$(find "$DIST" -type f \
  \( -name '*.rs' -o -name '*.swift' -o -name '*.toml' -o -name '*.lock' \
     -o -name '*.plist' -o -name '*.sh' -o -name '*.a' -o -name '*.entitlements' \) || true)
if [[ -n "$leaked" ]]; then
  echo "audit: FAIL — product source files in the published site:" >&2
  echo "$leaked" | sort -u >&2
  status=1
else
  echo "  ok  no product source files in the build"
fi

# Marker strings that only appear in the private tree or in CI configuration. A published page
# containing any of these means something read further than it should have.
markers='HOMEBREW_TAP_TOKEN|DOPPLER_TOKEN|APPLE_ID_PASSWORD|clowder_proto|ghostty_surface_'
matches=$(grep -rIoE "$markers" "$DIST" || true)
if [[ -n "$matches" ]]; then
  echo "audit: FAIL — private markers in the published site:" >&2
  echo "$matches" | sort -u >&2
  status=1
else
  echo "  ok  no private source markers in the build"
fi
```

- [ ] **Step 2: Add the `check_file` helper and fixtures to `site/scripts/audit-selftest.sh`**

The existing `check()` always writes `index.html`, so it cannot express "a file named `main.rs`
exists". Add this helper after it:

```bash
# Build a fixture dir containing a file named $3 (with innocuous content), then assert audit.sh's
# exit status matches $1. check() above always writes index.html, so it cannot express a fixture
# whose *filename* is the violation.
check_file() {
  local want="$1" name="$2" filename="$3"
  local dir="$WORK/$name"
  mkdir -p "$dir"
  printf '<html></html>\n' > "$dir/index.html"
  printf 'nothing to see here\n' > "$dir/$filename"

  local got
  if "$AUDIT" "$dir" >/dev/null 2>&1; then got=pass; else got=fail; fi

  if [[ "$got" == "$want" ]]; then
    echo "  ok    $name (audit $got, as expected)"
    pass=$((pass + 1))
  else
    echo "  FAIL  $name — expected audit to $want, but it $got" >&2
    fail=$((fail + 1))
  fi
}
```

Then add the cases before the final `echo`:

```bash
# --- check 3: private-source leakage -----------------------------------------
check_file fail leaked-rust    'main.rs'
check_file fail leaked-swift   'App.swift'
check_file fail leaked-cargo   'Cargo.toml'
check_file fail leaked-script  'build-app.sh'
check_file fail leaked-plist   'Info.plist'

# Files the site legitimately publishes must not trip it. .webmanifest and .xml in particular are
# real build outputs (manifest.webmanifest.ts and the sitemap integration).
check_file pass site-manifest  'manifest.webmanifest'
check_file pass site-sitemap   'sitemap-index.xml'
check_file pass site-css       'style.css'

check fail leaked-token-name \
  '<script>const t = "HOMEBREW_TAP_TOKEN";</script>'
check fail leaked-rust-symbol \
  '<p>uses clowder_proto internally</p>'

# The product NAME is obviously fine — only the internal symbols are not.
check pass product-name-is-fine \
  '<h1>Clowder</h1><p>a terminal for coding agents</p>'
```

- [ ] **Step 3: Run the self-test and confirm the new cases fail before check 3 exists**

If you added the fixtures first (recommended), run:

```bash
cd site && ./scripts/audit-selftest.sh; cd ..
```

Expected before Step 1's change: the `leaked-*` cases report `FAIL … expected audit to fail, but it
passed`. After Step 1: `audit-selftest: 19 passed, 0 failed` (8 existing + 11 new).

- [ ] **Step 4: Run it against a real build**

```bash
cd site && npm run build && cd ..
```

Expected: the build ends with `audit: passed`, now listing five `ok` lines. If `no product source
files` fails on a real build, a legitimate output has an extension on the list — narrow the list
rather than deleting the check.

- [ ] **Step 5: Commit**

```bash
git add site/scripts/audit.sh site/scripts/audit-selftest.sh
git commit -m "fix(site): guard against publishing private source

The site now builds inside the private product repo. dist/ carries no
provenance, so this checks for what leaked source would look like:
product source extensions, and marker strings that must never be
published. Milestone 2 reads scripts/lib/product.sh from outside site/,
so the boundary is about to be crossed deliberately."
```

---

### Task 8: Document the new layout and its hazards

**Files:**
- Modify: `AGENTS.md` (repo layout table; Build & test; Gotchas; CI)
- Modify: `site/README.md`
- Modify: `README.md`

- [ ] **Step 1: Add `site/` to the repo layout table in `AGENTS.md`**

Add a row after the `macos/` row:

```markdown
| `site/` | The public marketing site for `getclowder.app` (Astro, deployed to GitHub Pages). Ubuntu-only CI; **never** link to this repo from it — it is private and `site/scripts/audit.sh` fails the build on such a link | — |
```

- [ ] **Step 2: Add a site section under Build & test in `AGENTS.md`**

```markdown
**Site** (run inside `site/`):

```sh
cd site && npm ci
cd site && npm run dev      # http://localhost:4321
cd site && npm run check    # type-check .astro and .ts — `astro build` does NOT type-check
cd site && npm run build    # → dist/, then scripts/audit.sh
cd site && npm test         # scripts/audit-selftest.sh
```

The site needs no Rust, no Swift and no libghostty. `npm run check` is the only thing that catches a
type error — `astro build` strips types and exits 0 on one.
```

- [ ] **Step 3: Add the CI hazards to the Gotchas section in `AGENTS.md`**

```markdown
- **Site-only changes skip the macOS build.** `scripts/changed-scope.sh` classifies a range as
  `product` or `site-only` (allowlist: only `site/**` is cheap — `docs/` is not), and the macOS job
  is conditional on it. Verify a change to the rule with `scripts/changed-scope.sh --self-test`.
- **The required check is a gate job, not the macOS job.** `required-build-gate` in `ci.yml` carries
  the name `build + test (macOS, unsigned)`; the real job is `… — full`. Do **not** "simplify" this
  into two jobs sharing the required name: an `if:`-skipped job still files a check run with
  conclusion `skipped`, `check-runs-state.sh` picks the latest run per required name and counts
  `skipped` as failed, so the release merge gate would fail on a race. The name string is what
  `main`'s ruleset matches, so it must not change.
- **`deploy-site.yml` must never gain a `pull_request` trigger.** It holds `pages: write` and
  `id-token: write`, in the same repo as `DOPPLER_TOKEN` and the signing path.
```

- [ ] **Step 4: Add the site workflows to the CI section in `AGENTS.md`**

```markdown
- `.github/workflows/deploy-site.yml` (**Deploy site to GitHub Pages**) — pushes to `main` touching
  `site/**`, a daily `schedule:`, and `workflow_dispatch` (which `release.yml` fires after the tap
  publish, so a release does not wait for the daily run). Never on pull requests.
```

- [ ] **Step 5: Update `site/README.md`**

Replace the `## Develop` command block so the commands work from the new location, and add a line
under it:

```markdown
This site lives inside the private `clowder` repo under `site/`. It has its own `package.json` and
CI job and needs no Rust, Swift or libghostty toolchain. The two rules below are enforced by
`scripts/audit.sh`, which also refuses to publish product source files or private marker strings —
see `scripts/audit-selftest.sh` for exactly what that means.
```

Also correct the "How it stays current" section: `deploy.yml` is now
`.github/workflows/deploy-site.yml` at the **repo root**, not in this directory.

- [ ] **Step 6: Add a pointer in the root `README.md`**

One line in whatever section lists the repo's parts:

```markdown
The marketing site for [getclowder.app](https://getclowder.app) lives in [`site/`](site/) — see
[`site/README.md`](site/README.md).
```

- [ ] **Step 7: Verify every documented command actually works**

```bash
cd site && npm run check && npm test && cd ..
scripts/changed-scope.sh --self-test
```

Expected: all pass. Do not document a command you have not just run.

- [ ] **Step 8: Commit**

```bash
git add AGENTS.md README.md site/README.md
git commit -m "docs: document the site's new home and its CI hazards

Records why the required check is a gate job rather than two jobs
sharing a name, since that is the change most likely to be 'simplified'
back into a release-breaking one."
```

---

## Done means

- [ ] `cd site && npm ci && npm run check && npm run build && npm test` all pass
- [ ] `scripts/changed-scope.sh --self-test` passes; `scripts/check-runs-state.sh --self-test` still passes
- [ ] `scripts/check-commit-messages.sh` passes over the whole branch
- [ ] On a **site-only** commit: exactly one check run named `build + test (macOS, unsigned)`, with
      `conclusion: success`, and **no macOS job in the run**
- [ ] On a **product** commit: the same single check run, success, with the macOS job having actually run
- [ ] `scripts/check-runs-state.sh --sha <head>` reports `passed` for both cases
- [ ] `clowder-site` still serves `getclowder.app`, untouched

## Explicitly NOT in this milestone

The **domain cutover** — disabling Pages on `clowder-site`, enabling it here, moving the custom
domain, re-ticking Enforce HTTPS, archiving the old repo. It needs GitHub UI access, has real
downtime while the certificate re-provisions, and cannot run in parallel with the old site because a
custom domain maps to exactly one repository. Landing this milestone first means a bad migration is a
revert rather than an outage.

Sequence it after this merges: dispatch `deploy-site.yml`, compare its artifact against the live
site, then cut over.
