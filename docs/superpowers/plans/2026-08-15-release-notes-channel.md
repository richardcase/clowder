# Release-Notes Channel (Milestone 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Clowder a public record of what changed in each release — written per-change while the
context is fresh, collected at release, published to the Homebrew tap, and rendered at `/whats-new`.

**Architecture:** Fragments accumulate in `site/src/content/unreleased/`. A required CI check makes a
PR containing `feat`/`fix` add one. At release, the `bump` job collects them into
`site/src/content/releases/vX.Y.Z.md` and commits that through the existing GraphQL signed-commit
mutation — whose hardcoded file list is generalized to do it. `update-homebrew-tap.sh` publishes the
result as the tap release body; an Astro content collection renders it.

**Tech Stack:** bash (`scripts/`, self-tested), GitHub Actions, Astro 7 content collections
(`glob` loader), GraphQL `createCommitOnBranch`.

**Spec:** `docs/superpowers/specs/2026-08-15-clowder-site-monorepo-freshness-design.md` — Milestone 1.
Part E of that milestone (the cutover/Cloudflare doc corrections) is already committed as `c373bfb`.

## Global Constraints

- **This repo is private; the site is public.** Never introduce `github.com/defiantsoftware/clowder`
  into `site/` output — `site/scripts/audit.sh` fails the build on it. Public links go to the tap,
  `https://github.com/defiantsoftware/homebrew-clowder`.
- **Every commit must be signed.** `main`'s ruleset has `required_signatures` with no bypass actors.
  Ordinary `git commit` signs (global `commit.gpgsign=true`, ssh format). **Never use
  `git filter-branch`** — it strips signatures silently. `git log --format=%G?` reports `N` on a
  correctly signed commit here because `gpg.ssh.allowedSignersFile` is unset; check with
  `git cat-file commit <sha> | grep -q gpgsig` instead.
- **Commit messages are Conventional Commits.** Run `scripts/check-commit-messages.sh` before pushing.
- **Never hardcode a `/clowder-site/` prefix.** The site serves from the root of `getclowder.app`.
- **The required check context is exactly `build + test (macOS, unsigned)`** and lives on the
  `required-build-gate` job. Do not touch it.
- **`scripts/check-runs-state.sh` treats `skipped`/`neutral` as FAILED.**
- Fragments are a **sibling** of the collection directory, never nested inside it.
- Work on branch `richardcase/release-notes`. Do not commit to `main`. Do not `git push`.

---

### Task 1: `scripts/check-release-notes.sh` and its CI wiring

**Files:**
- Create: `scripts/check-release-notes.sh`
- Modify: `.github/workflows/ci.yml` (the `commit-lint` job only)

**Interfaces:**
- Consumes: `scripts/lib/conventional.sh` — provides `CC_TYPES`, `CC_PATTERN`, `cc_subject_ok`, and
  `cc_parse` (sets `CC_TYPE`, `CC_SCOPE`, `CC_BREAKING`, `CC_DESC`; returns 1 if unparseable).
- Produces: `scripts/check-release-notes.sh [<base> [<head>]]` exits 0 when the range needs no note or
  a note was added; non-zero otherwise. `--file <path>` guards a single file's content and exits
  non-zero on a violation. `--self-test` exits 0 when all cases pass.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Two jobs, both about the release notes under site/src/content/:
#
#   1. Require a note. A pull request containing any `feat` or `fix` commit must add a fragment to
#      site/src/content/unreleased/, or carry the `no-release-note` label. Per PULL REQUEST, not per
#      commit: three fixes need one note, not three.
#
#   2. Guard the content. This repo is PRIVATE and the site is PUBLIC. Today's release bodies are
#      full of `richardcase/clowder` PR links that 404 for every visitor and scope names like `m12b`
#      that mean nothing to one. Notes cross that boundary, so they are checked at the crossing.
#
# --self-test exists because a grep that matches nothing exits 0 and looks like a pass. That is the
# same reasoning that puts next-version.sh --self-test and check-runs-state.sh --self-test in the
# same CI job; check 1 of site/scripts/audit.sh actually regressed that way once.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/conventional.sh
. "$ROOT/scripts/lib/conventional.sh"

FRAGMENT_DIR='site/src/content/unreleased'
LABEL='no-release-note'

# Patterns that must never reach a public page. Kept as one alternation so the failure message can
# show exactly what matched.
#   - the private repo under either owner (the org moved; both forms exist in old release bodies)
#   - `#123` PR/issue references, which resolve to nothing public
#   - internal milestone scopes: m7d, m10c, m11a, m12b
FORBIDDEN='github\.com/richardcase/clowder|github\.com/defiantsoftware/clowder([/"?#]|$)|#[0-9]+|\bm[0-9]+[a-z]?\b'

die() { echo "error: $*" >&2; exit 2; }

# guard_file <path> -> 0 clean, 1 violation
guard_file() {
  local f="$1" hits
  [ -f "$f" ] || die "no such file: $f"
  hits="$(grep -nIoE "$FORBIDDEN" "$f" || true)"
  if [ -n "$hits" ]; then
    echo "release-notes: FAIL — $f contains references that are not public:" >&2
    echo "$hits" | sed 's/^/  /' >&2
    cat >&2 <<'EOF'

  This repo is private and the site is public. Pull request numbers and links to the source repo
  404 for every visitor, and milestone scopes like `m12b` mean nothing to one. Describe the change
  in plain language instead.
EOF
    return 1
  fi
  return 0
}

# needs_note <base> <head> -> 0 if the range contains a feat or fix
needs_note() {
  local subject
  while IFS= read -r subject; do
    cc_parse "$subject" || continue
    case "$CC_TYPE" in
      feat | fix) return 0 ;;
    esac
  done < <(git log --no-merges --format=%s "$1..$2")
  return 1
}

# added_fragments <base> <head> -> prints added fragment paths
added_fragments() {
  git diff --name-only --diff-filter=A "$1" "$2" -- "$FRAGMENT_DIR" || true
}

self_test() {
  local pass=0 fail=0 tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  check_guard() {
    local want="$1" name="$2" body="$3" f="$tmp/$name.md" got
    printf '%s\n' "$body" > "$f"
    if guard_file "$f" >/dev/null 2>&1; then got=clean; else got=violation; fi
    if [ "$got" = "$want" ]; then
      echo "  ok    $name ($got)"
      pass=$((pass + 1))
    else
      echo "  FAIL  $name — wanted $want, got $got" >&2
      fail=$((fail + 1))
    fi
  }

  echo "check-release-notes: verifying the content guard"

  # Must be rejected — these are the exact shapes today's release bodies contain.
  check_guard violation old-owner-link   'See https://github.com/richardcase/clowder/pull/72 for detail.'
  check_guard violation new-owner-link   'Source: https://github.com/defiantsoftware/clowder'
  check_guard violation owner-link-path  'https://github.com/defiantsoftware/clowder/issues/1'
  check_guard violation pr-reference     'Fixed the pane resize bug (#82).'
  check_guard violation milestone-scope  'Landed as part of m11a.'
  check_guard violation milestone-plain  'The m7d work is complete.'

  # Must be accepted — the public repos share a prefix with the private one, and ordinary prose
  # about the product must not trip the guard.
  check_guard clean tap-link       'Install with the tap at https://github.com/defiantsoftware/homebrew-clowder'
  check_guard clean site-link      'https://github.com/defiantsoftware/clowder-site is the old site repo'
  check_guard clean plain-prose    'Connect the app to a Clowder daemon on another machine over TLS.'
  check_guard clean version-number 'Requires macOS 14 or later.'
  check_guard clean hash-in-word   'The colour is set with a hex triplet.'

  echo "check-release-notes: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit $? ;;
  --file)
    [ "$#" -eq 2 ] || die "--file takes exactly one path"
    guard_file "$2" || exit 1
    echo "release-notes: ok  $2"
    exit 0
    ;;
  -h | --help)
    cat <<'EOF'
Usage: scripts/check-release-notes.sh [<base-ref> [<head-ref>]]   (default: origin/main HEAD)
       scripts/check-release-notes.sh --file <path>               (guard one file's content)
       scripts/check-release-notes.sh --self-test
EOF
    exit 0
    ;;
esac

cd "$ROOT"

BASE="${1:-origin/main}"
HEAD_REF="${2:-HEAD}"

for ref in "$BASE" "$HEAD_REF"; do
  git rev-parse --verify --quiet "${ref}^{commit}" >/dev/null \
    || die "'$ref' does not resolve to a commit (a shallow clone may not contain it; CI uses fetch-depth: 0)"
done

merge_base="$(git merge-base "$BASE" "$HEAD_REF")" || die "'$BASE' and '$HEAD_REF' have no common ancestor"

# Guard every fragment and collected note the range touches, added or modified. Content is checked
# even when no note is required — a `docs:`-only PR editing a note must still not leak.
touched="$(git diff --name-only --diff-filter=AM "$merge_base" "$HEAD_REF" \
  -- "$FRAGMENT_DIR" 'site/src/content/releases' || true)"
guard_status=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  guard_file "$f" || guard_status=1
done <<< "$touched"
[ "$guard_status" -eq 0 ] || exit 1

if ! needs_note "$merge_base" "$HEAD_REF"; then
  echo "release-notes: ok  no feat or fix in this range — no note required"
  exit 0
fi

if [ -n "$(added_fragments "$merge_base" "$HEAD_REF")" ]; then
  echo "release-notes: ok  fragment added"
  exit 0
fi

# The label is read from the environment so this stays testable and needs no `gh` call. CI passes it
# from the pull request payload; there is no label outside a pull request, which is correct — the
# bump commit is `chore:` and so never reaches here.
case ",${PR_LABELS:-}," in
  *",$LABEL,"*)
    echo "release-notes: ok  '$LABEL' label present — skipping by explicit choice"
    exit 0
    ;;
esac

cat >&2 <<EOF
release-notes: FAIL — this pull request has a feat or fix commit but adds no release note.

  Add one file to $FRAGMENT_DIR/<slug>.md describing, in one or two sentences, what a user can now
  do that they could not before. Plain language, one capability per file:

      Connect the app to a Clowder daemon on another machine over TLS.

  Not a change record — no CLI surface dumps, no pull request numbers, no milestone scopes.

  If this change is genuinely internal and no user could perceive it — a CI fix, a refactor of the
  release tooling — DO NOT invent a note for it. Add the '$LABEL' label to this pull request and
  re-run this job. That is what the label is for, and a filler note is worse than no note.
EOF
exit 1
```

- [ ] **Step 2: Run the self-test**

```bash
chmod +x scripts/check-release-notes.sh
scripts/check-release-notes.sh --self-test
```

Expected: `check-release-notes: 12 passed, 0 failed`. If a case fails, fix the pattern — never the
expectation.

- [ ] **Step 3: Verify the guard against the real worst-case input**

The whole point is catching what today's release bodies contain. Confirm it does:

```bash
gh release view v0.6.0 -R defiantsoftware/clowder --json body --jq .body > /tmp/real-notes.md
scripts/check-release-notes.sh --file /tmp/real-notes.md; echo "exit=$?"
```

Expected: **exit 1**, listing `richardcase/clowder` links and `m11a`/`m12b`-style tokens. A pass here
means the guard is broken.

- [ ] **Step 4: Wire it into `commit-lint`**

In `.github/workflows/ci.yml`, in the `commit-lint` job, after the `changed-scope.sh self-tests` step:

```yaml
      # Same reasoning as the self-tests above, and one more: this guard is what stops a private
      # repo link or an internal milestone scope reaching a public page, so a green run has to be
      # evidence it actually ran.
      - name: check-release-notes.sh self-tests
        run: scripts/check-release-notes.sh --self-test

      - name: Release notes
        env:
          BASE: ${{ github.event.pull_request.base.sha || 'origin/main' }}
          HEAD_REF: ${{ github.event.pull_request.head.sha || github.sha }}
          # Comma-joined so the script needs no `gh` call and stays unit-testable. Empty outside a
          # pull request, which is correct: the bump commit is `chore:` and requires no note.
          PR_LABELS: ${{ join(github.event.pull_request.labels.*.name, ',') }}
        run: scripts/check-release-notes.sh "$BASE" "$HEAD_REF"
```

- [ ] **Step 5: Verify the YAML and the job's step list**

```bash
ruby -ryaml -e 'd=YAML.load_file(".github/workflows/ci.yml"); d["jobs"]["commit-lint"]["steps"].each{|s| puts "- #{s["name"] || s["uses"]}"}'
```

Expected: checkout, Check commit messages, next-version self-tests, check-runs-state self-tests,
changed-scope self-tests, check-release-notes self-tests, Release notes.

- [ ] **Step 6: Commit**

```bash
git add scripts/check-release-notes.sh .github/workflows/ci.yml
git commit -m "ci: require a release note on feat and fix pull requests

Notes cross a private-to-public boundary, so the same script guards
their content: today's release bodies are full of source-repo links that
404 for visitors and milestone scopes that mean nothing to one.

Per pull request rather than per commit, and with a no-release-note
label for genuinely internal changes — the failure message says so
outright, because requiring a note on every fix otherwise produces
filler."
```

---

### Task 2: Content collection and the backfill

Content before renderer, so Task 3 has something real to build against.

**Files:**
- Create: `site/src/content.config.ts`
- Create: `site/src/content/releases/{initial-release,v0.4.0,v0.5.0,v0.6.0}.md`
- Create: `site/src/content/unreleased/.gitkeep`

**Interfaces:**
- Consumes: `scripts/check-release-notes.sh --file` from Task 1.
- Produces: a `releases` collection with `z.object({ version: z.string(), date: z.string() })`. Task 3
  reads it via `getCollection('releases')`.

- [ ] **Step 1: Create the collection config**

```ts
import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

// Only *.md at this exact directory — NOT `**/*.md`. In-flight fragments live in the sibling
// src/content/unreleased/ and must never render as if they had shipped. The sibling layout is what
// makes that structural rather than a property of this pattern, but keep the pattern narrow anyway.
const releases = defineCollection({
  loader: glob({ base: './src/content/releases', pattern: '*.md' }),
  schema: z.object({
    /** Semver without the leading `v`, e.g. `0.6.0`. Sorted numerically, never as a string. */
    version: z.string(),
    /** ISO date, e.g. `2026-08-12`. */
    date: z.string(),
  }),
});

export const collections = { releases };
```

- [ ] **Step 2: Keep the fragment directory in git**

Git does not track empty directories, and collection empties it at every release.

```bash
mkdir -p site/src/content/unreleased
printf '%s\n' \
  '# Release-note fragments live here, one file per user-facing change.' \
  '#' \
  '# One or two sentences describing what a user can now do that they could not before.' \
  '# Plain language, one capability per file. No CLI surface dumps, no pull request numbers,' \
  '# no milestone scopes — scripts/check-release-notes.sh rejects those.' \
  '#' \
  '# scripts/collect-release-notes.sh consumes them at release time and deletes them; this file' \
  '# keeps the directory in git afterwards.' \
  > site/src/content/unreleased/.gitkeep
```

- [ ] **Step 3: Gather the source material**

```bash
for t in v0.4.0 v0.5.0 v0.6.0; do
  echo "===== $t ====="
  gh release view "$t" -R defiantsoftware/clowder --json body --jq .body
done
git log --no-merges --format='%s' v0.3.0 | head -40   # for the initial-release entry
```

- [ ] **Step 4: Write the four entries**

Rewrite the PR titles into user-facing prose. **Do not paste them.** They contain
`richardcase/clowder` links and `m11a`-style scopes, which the guard rejects — that is the point.

Each file: frontmatter, then `- ` bullets. One capability per bullet, plain language, aimed at
someone deciding whether to upgrade.

`site/src/content/releases/initial-release.md` — one entry for 0.1.0–0.3.0, all shipped 2026-07-31:

```markdown
---
version: '0.3.0'
date: '2026-07-31'
---

The first public builds of Clowder: a native macOS terminal that runs a fleet of coding agents, each
in its own git worktree, and tells you which one needs you.

- Run Claude Code, OpenAI Codex or a plain shell against any git or jj project, all in one window.
- Every agent gets its own worktree or workspace, created outside your repository, so your checkout
  is never touched.
- Attention routing: badges and a menu-bar count tell you which agent is waiting on you.
- Real terminals rendered natively, with splits and a command palette.
```

Then `v0.4.0.md`, `v0.5.0.md`, `v0.6.0.md` with their real `version`/`date` (take dates from
`git log -1 --format=%ad --date=short <tag>`), each a handful of bullets drawn from that release's
body. 0.6.0's headline items are remote hosts, agent profiles, terminal copy/paste, and pane
environments picking up your login shell's `PATH`.

- [ ] **Step 5: Guard every file — this is the real test of Task 1**

```bash
for f in site/src/content/releases/*.md; do scripts/check-release-notes.sh --file "$f" || exit 1; done
```

Expected: `release-notes: ok` for each. A failure means a PR link or milestone scope survived the
rewrite — fix the prose, never the guard.

- [ ] **Step 6: Confirm Astro loads the collection**

```bash
cd site && npm run check && cd ..
```

Expected: 0 errors. A schema mismatch surfaces here.

- [ ] **Step 7: Commit**

```bash
git add site/src/content.config.ts site/src/content
git commit -m "feat(site): add the releases content collection and backfill it

0.1.0, 0.2.0 and 0.3.0 all shipped on the same day and 0.3.0's release
body has no bullets at all, so they collapse into one initial-release
entry rather than three identically dated ones on a page whose job is
signalling rhythm.

Fragments live in a sibling directory, not inside the collection: nested
would work only while the glob pattern stays *.md, and a later change to
**/*.md would publish in-flight notes as shipped releases."
```

---

### Task 3: Render — Nav, `/whats-new`, and the homepage band

**Files:**
- Modify: `site/src/components/Nav.astro`
- Create: `site/src/pages/whats-new.astro`
- Create: `site/src/components/WhatsNew.astro` (the homepage band)
- Modify: `site/src/pages/index.astro`

**Interfaces:**
- Consumes: the `releases` collection from Task 2 via `getCollection('releases')`.

- [ ] **Step 1: Make the Nav work off the homepage**

`Nav.astro` currently links `#top`, `#features`, `#architecture`, `#faq` and `#install`. On
`/whats-new` every one of those is a dead same-page anchor. Make all five root-relative (`/#features`
and so on) and add a What's new link:

```html
    <div class="nav__links">
      <a href="/#features">Features</a>
      <a href="/#architecture">How it works</a>
      <a href="/#faq">FAQ</a>
      <a href="/whats-new">What's new</a>
    </div>
```

Also update the mobile-wrap comment in that file's `<style>` block: it says "three in-page anchors do
not justify a new interactive component". It is now four links, one of which is a route, not an
anchor. Keep the conclusion, correct the count.

- [ ] **Step 2: Verify in-page scrolling still works — the regression risk**

```bash
cd site && npm run dev
```

From `/`, click Features, How it works, FAQ, the brand mark and Install: each must scroll in-page,
not reload. From `/whats-new`, each must navigate to the homepage and land on the right section.

- [ ] **Step 3: Build `/whats-new`**

`site/src/pages/whats-new.astro` — use `Base`, `Nav` and `Footer` exactly as `index.astro` does.

```astro
---
import { getCollection, render } from 'astro:content';
import Base from '../layouts/Base.astro';
import Nav from '../components/Nav.astro';
import Footer from '../components/Footer.astro';
import { site } from '../data/site';

// Numeric, not lexicographic: string order puts 0.9.0 above 0.10.0.
const key = (v: string) => v.split('.').map(Number);
const cmp = (a: string, b: string) => {
  const [x, y] = [key(a), key(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (y[i] ?? 0) - (x[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
};

const entries = await getCollection('releases');
const releases = entries.sort((a, b) => cmp(a.data.version, b.data.version));
const rendered = await Promise.all(releases.map(async (r) => ({ ...r, Body: (await render(r)).Content })));
---
```

Render newest first: version heading, the date in a `<time datetime={...}>`, then the body. Match the
type scale and spacing of the existing components (`Features.astro` is the closest model). Page title
`What's new — ${site.name}`.

- [ ] **Step 4: Build the homepage band**

`site/src/components/WhatsNew.astro`: the newest release's version and its first two or three
bullets, linking to `/whats-new`.

**No date.** The mechanism that signals a healthy cadence signals neglect the moment releases pause,
automatically and with nobody deciding to. Put that reasoning in a comment at the top of the file,
in the style of the other components' headers, so nobody "improves" it by adding one.

Insert into `index.astro` after `<Features />` and before `<Screenshots />`.

- [ ] **Step 5: Verify sorting numerically**

```bash
cd site && cat > /tmp/sorttest.md <<'EOF'
---
version: '0.10.0'
date: '2026-12-01'
---

- Sort probe.
EOF
cp /tmp/sorttest.md src/content/releases/v0.10.0.md && npm run dev
```

`/whats-new` must show 0.10.0 **above** 0.9.0/0.6.0. Then `rm src/content/releases/v0.10.0.md`.

- [ ] **Step 6: Full build**

```bash
cd site && npm run check && npm test && npm run build && cd ..
```

Expected: 0 errors; `audit-selftest: 19 passed`; `audit: passed`.

- [ ] **Step 7: Commit**

```bash
git add site/src
git commit -m "feat(site): add a what's-new page and homepage band

The nav anchors had to become root-relative first: the site was a single
page, so every #features-style link was dead from a real route.

The band carries no date on purpose. The same mechanism that signals a
healthy cadence signals neglect the moment releases pause, with nobody
deciding to — a version and a couple of items degrade to uninformative
instead. Dates stay on /whats-new, where a changelog without them would
be useless."
```

---

### Task 4: `scripts/collect-release-notes.sh`

**Files:**
- Create: `scripts/collect-release-notes.sh`

**Interfaces:**
- Consumes: `scripts/check-release-notes.sh --file` from Task 1.
- Produces: `scripts/collect-release-notes.sh <version>` writes
  `site/src/content/releases/v<version>.md` and deletes `site/src/content/unreleased/*.md`.
  `--self-test` exits 0 when all cases pass.

- [ ] **Step 1: Write the script**

Requirements, each of which needs a self-test case:

- Concatenates `site/src/content/unreleased/*.md` in sorted filename order into
  `site/src/content/releases/v<version>.md`, with frontmatter `version: '<version>'` and
  `date: '<YYYY-MM-DD>'` (override the date with `RELEASE_DATE` so the self-test is deterministic).
- Deletes the consumed fragments. **Never deletes `.gitkeep`.**
- **No fragments → writes `Maintenance and fixes.` and exits 0.** A patch release of pure `fix`
  commits is legitimate and must not block a release.
- A missing `unreleased/` directory behaves the same as an empty one.
- Runs `check-release-notes.sh --file` on the result and fails if it does not pass.
- Refuses a version that is not `X.Y.Z[-prerelease]`, matching `set-version.sh`'s own validation.
- Idempotent-ish: refuses to overwrite an existing `v<version>.md` unless `--force`, so a re-dispatch
  cannot silently double-collect.

- [ ] **Step 2: Run the self-test**

```bash
chmod +x scripts/collect-release-notes.sh
scripts/collect-release-notes.sh --self-test
```

Expected: all cases pass, including the no-fragments case producing `Maintenance and fixes.` and the
`.gitkeep` survival case.

- [ ] **Step 3: Real dry run**

```bash
printf -- '- Probe one.\n' > site/src/content/unreleased/probe-one.md
printf -- '- Probe two.\n' > site/src/content/unreleased/probe-two.md
RELEASE_DATE=2026-08-15 scripts/collect-release-notes.sh 0.6.1
cat site/src/content/releases/v0.6.1.md
ls site/src/content/unreleased/
```

Expected: both bullets in order, valid frontmatter, fragments gone, `.gitkeep` still present. Then
`rm site/src/content/releases/v0.6.1.md`.

- [ ] **Step 4: Commit**

```bash
git add scripts/collect-release-notes.sh
git commit -m "ci: collect release-note fragments into a per-version file

No fragments writes 'Maintenance and fixes.' rather than failing: a
patch release of pure fix commits is legitimate and must not block a
release. Refuses to overwrite an existing version file without --force,
so a re-dispatched release cannot double-collect."
```

---

### Task 5: `scripts/gh-file-changes.sh` and the bump wiring

The only task that touches the signed-commit path. Read the whole task before starting.

**Files:**
- Create: `scripts/gh-file-changes.sh`
- Modify: `.github/workflows/release.yml` (the `bump` job's `Commit the bump` step, ~line 194-219)

**Interfaces:**
- Produces: `scripts/gh-file-changes.sh [<ref>]` prints a JSON object
  `{"additions":[{"path":…,"contents":<base64>}],"deletions":[{"path":…}]}` describing the working
  tree's changes against `<ref>` (default `HEAD`). `--self-test` exits 0 when all cases pass.

- [ ] **Step 1: Read what you are replacing**

```bash
sed -n '191,220p' .github/workflows/release.yml
```

The current step hardcodes three additions and has **no `deletions` field**. It uses
`createCommitOnBranch` rather than `git commit` because **GitHub signs commits made by that
mutation** and `main`'s ruleset requires signatures. Do not change that mechanism.

- [ ] **Step 2: Write the script**

Requirements, each needing a self-test case:

- Enumerates changes with `git status --porcelain=v1 -z` (NUL-delimited — paths may contain spaces),
  covering added, modified and deleted files, staged or not, plus untracked files.
- Additions carry base64 of the file's bytes, single-line. `bump` runs on `ubuntu-latest`, so
  `base64 -w0` is available; do not silently depend on that elsewhere.
- Deletions carry only the path.
- Emits valid JSON via `jq`, never string concatenation.
- Empty change set emits `{"additions":[],"deletions":[]}` rather than failing — the caller decides.
- Rejects a rename by treating it as delete + add, so no `R` status leaks through unhandled.

- [ ] **Step 3: Self-test, then prove it against the real bump**

```bash
chmod +x scripts/gh-file-changes.sh
scripts/gh-file-changes.sh --self-test
```

Then the case that matters — a real bump, notes and all:

```bash
git checkout -q -b tmp/bump-probe
printf -- '- Probe.\n' > site/src/content/unreleased/probe.md
git add -A && git commit -q -m "chore: probe fragment"
scripts/set-version.sh 0.6.1 >/dev/null
RELEASE_DATE=2026-08-15 scripts/collect-release-notes.sh 0.6.1
scripts/gh-file-changes.sh | jq '{additions: [.additions[].path], deletions: [.deletions[].path]}'
```

Expected additions: `VERSION`, `Cargo.toml`, `Cargo.lock`, `site/src/content/releases/v0.6.1.md`.
Expected deletions: `site/src/content/unreleased/probe.md`. Then:

```bash
git checkout -q richardcase/release-notes && git branch -D tmp/bump-probe && git checkout -- .
```

- [ ] **Step 4: Validate the JSON against the mutation's shape before trusting it**

```bash
scripts/gh-file-changes.sh | jq -e '
  (.additions | type == "array") and
  (.deletions | type == "array") and
  (all(.additions[]; has("path") and has("contents") and (.contents | test("^[A-Za-z0-9+/=]*$")))) and
  (all(.deletions[]; has("path") and (has("contents") | not)))
' && echo "shape ok"
```

- [ ] **Step 5: Rewire the bump step**

Replace the hardcoded `jq -n --arg v/--arg ct/--arg cl` block so the mutation takes its file list from
the script. `set-version.sh` runs first, then `collect-release-notes.sh`, then the commit:

```yaml
      - name: Collect the release notes
        run: scripts/collect-release-notes.sh "$VERSION"

      # createCommitOnBranch is used rather than `git commit` or the REST git-data API because
      # GitHub SIGNS commits made by this mutation, and main's ruleset requires signed commits.
      # (The REST API's `signature` field is caller-supplied — it does not sign for you.)
      #
      # The file list comes from the working tree rather than being hardcoded: collecting notes
      # DELETES fragments, which the old three-addition literal could not express at all, and a
      # hand-maintained list silently drops anything new that set-version.sh starts touching.
      - name: Commit the bump
        id: commit
        run: |
          jq -n \
            --arg repo "$GITHUB_REPOSITORY" \
            --arg branch "$BRANCH" \
            --arg oid "$BASE_SHA" \
            --arg headline "chore: v$VERSION" \
            --argjson changes "$(scripts/gh-file-changes.sh)" \
            '{
              query: "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
              variables: { input: {
                branch: { repositoryNameWithOwner: $repo, branchName: $branch },
                expectedHeadOid: $oid,
                message: { headline: $headline },
                fileChanges: $changes
              }}
            }' > "$RUNNER_TEMP/commit.json"
          sha="$(gh api graphql --input "$RUNNER_TEMP/commit.json" --jq .data.createCommitOnBranch.commit.oid)"
          echo "head_sha=$sha" >> "$GITHUB_OUTPUT"
```

**Leave the `Verify GitHub signed the commit` step exactly as it is** — it is what catches a
malformed mutation before the merge does.

- [ ] **Step 6: Verify the YAML and step order**

```bash
ruby -ryaml -e 'd=YAML.load_file(".github/workflows/release.yml"); d["jobs"]["bump"]["steps"].each{|s| puts "- #{s["name"] || s["uses"]}"}'
```

Expected: `set-version.sh` step, then Collect the release notes, then Commit the bump, then Verify
GitHub signed the commit.

- [ ] **Step 7: Commit**

```bash
git add scripts/gh-file-changes.sh .github/workflows/release.yml
git commit -m "ci: build the bump commit's file list from the working tree

Collecting release notes deletes fragments, which the hardcoded
three-addition literal had no deletions field to express. Generalizing
also removes a latent brittleness: the old list was hand-maintained, so
anything new set-version.sh started touching would have been silently
dropped from the bump commit.

createCommitOnBranch is unchanged — it is what makes GitHub sign the
commit, which main's ruleset requires."
```

---

### Task 6: Publish to the tap, and document

**Files:**
- Modify: `scripts/update-homebrew-tap.sh`
- Modify: `AGENTS.md`, `docs/versioning.md`

**Interfaces:**
- Consumes: `site/src/content/releases/v<version>.md` from Task 4;
  `check-release-notes.sh --file` from Task 1.

- [ ] **Step 1: Publish the notes**

In `scripts/update-homebrew-tap.sh`, before the release-create block, resolve the body:

```bash
# The site is the primary channel for these — it reads the same files locally — but the tap release
# is the only public record independent of it.
NOTES="$ROOT/site/src/content/releases/v$VERSION.md"
BODY="$(mktemp)"; trap 'rm -f "$BODY"' EXIT

if [ -f "$NOTES" ]; then
  "$ROOT/scripts/check-release-notes.sh" --file "$NOTES" >/dev/null
  # Strip the frontmatter block; publish the prose.
  awk 'BEGIN{n=0} /^---[[:space:]]*$/{n++; next} n>=2' "$NOTES" > "$BODY"
else
  # DELIBERATELY NOT FATAL. This runs at step 17 of the release job — AFTER step 14 tags the commit
  # and step 15 publishes the GitHub Release. Aborting here would strand a signed, notarized,
  # tagged, published release with no installable artifact on the tap, over a missing markdown
  # file. Enforcement belongs in the bump job, where failing costs nothing.
  echo "::warning::no release notes at $NOTES — publishing the stub body"
  printf 'Clowder %s\n' "$VERSION" > "$BODY"
fi
```

- [ ] **Step 2: Use it on BOTH paths**

Today notes are set only when the release is created, so a re-run silently keeps the stub — the same
idempotency the DMG upload already has via `--clobber`:

```bash
if ! gh release view "$TAG" --repo "$TAP_REPO" >/dev/null 2>&1; then
  gh release create "$TAG" --repo "$TAP_REPO" --title "$TAG" --notes-file "$BODY"
else
  gh release edit "$TAG" --repo "$TAP_REPO" --notes-file "$BODY"
fi
```

- [ ] **Step 3: Test against a throwaway public repo — run it twice**

```bash
gh repo create <you>/tap-probe --public --add-readme
dd if=/dev/zero of=/tmp/Clowder-0.6.1-macos.dmg bs=1k count=8
HOMEBREW_TAP_TOKEN=$(gh auth token) VERSION=0.6.1 DMG=/tmp/Clowder-0.6.1-macos.dmg \
  TAP_REPO=<you>/tap-probe scripts/update-homebrew-tap.sh
```

Run it **twice**. The second run must still show the notes — that is the create-only bug being fixed.
Then delete `site/src/content/releases/v0.6.1.md` and run again: it must **warn and publish the
stub**, exit 0, not fail. Finally `gh repo delete <you>/tap-probe --yes`.

- [ ] **Step 4: Document**

`AGENTS.md` — a Conventions bullet: every `feat`/`fix` PR adds a fragment to
`site/src/content/unreleased/`, one user-facing capability in plain language; the `no-release-note`
label is the escape for genuinely internal changes and labelling needs a manual job re-run; the
content guard exists because the repo is private and the site is public.

`docs/versioning.md` — the bump job now also collects notes, and the one-time setup section gains
`gh label create no-release-note -c BFD4F2 -d "Change needs no release note"` alongside the existing
`release` label.

- [ ] **Step 5: Commit**

```bash
git add scripts/update-homebrew-tap.sh AGENTS.md docs/versioning.md
git commit -m "ci: publish release notes to the tap release body

Warns and falls back to the stub when the notes file is missing rather
than failing: this runs after the tag and the GitHub Release, so
aborting would strand a signed release with nothing installable on the
tap. Enforcement lives in the bump job, where failing is free.

Also sets the notes on the already-exists path — they were only ever set
at create time, so a re-run silently kept the stub."
```

---

## Done means

- [ ] `scripts/check-release-notes.sh --self-test`, `collect-release-notes.sh --self-test`,
      `gh-file-changes.sh --self-test` all pass, and the three existing self-tests still do
- [ ] `scripts/check-release-notes.sh --file` **rejects** the real v0.6.0 release body
- [ ] Every backfilled note passes the guard
- [ ] `cd site && npm run check && npm test && npm run build` → 0 errors, 19/19, `audit: passed`
- [ ] `/whats-new` renders newest-first with 0.10.0 sorting above 0.9.0; every Nav link works from
      both `/` and `/whats-new`; the homepage band shows no date
- [ ] `gh-file-changes.sh` on a real bump emits the four additions and the fragment deletions
- [ ] `update-homebrew-tap.sh` run twice against a throwaway tap keeps the notes, and warns rather
      than failing when they are absent
- [ ] `scripts/check-commit-messages.sh` passes; every commit signed
      (`git cat-file commit <sha> | grep -q gpgsig`)

## Not in this milestone

The **one-time `gh label create no-release-note`** must be run against the real repo before the first
PR that needs the escape hatch. It is documented in Task 6 but is a repo setting, not code.

**End to end:** per `docs/versioning.md`, the first dispatch after any `release.yml` change **must be
a `prerelease` run**. An rc exercises Task 5 (collection inside the signed bump commit) but **not**
Task 6 — pre-releases skip `update-homebrew-tap.sh` entirely.
