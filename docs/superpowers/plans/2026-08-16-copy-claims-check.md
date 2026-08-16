# Copy-Claims Check (Milestone 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the site's limitation claims from going stale unnoticed, by failing a pull request when
a claim's tracking issue closes, or when a release note contradicts one.

**Architecture:** One new self-tested bash script, `scripts/check-copy-claims.sh`, wired into the
existing `commit-lint` job beside `check-release-notes.sh`. It reads `gap:` annotations from
`Faq.astro`, asks the issues API for their state, and compares open gaps' titles against release-note
fragments added in the pull request.

**Tech Stack:** bash (self-tested, must run under bash 3.2), `gh` CLI / GitHub issues API, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-08-15-clowder-site-monorepo-freshness-design.md`, Milestone 3
(replaced in commit `c083361`; the original design was vacuous and the spec records why).

## Global Constraints

- **This repo is private; the site is public.** Never introduce `github.com/defiantsoftware/clowder`
  into `site/` output — `site/scripts/audit.sh` fails the build on it.
- **Every commit must be signed.** `main`'s ruleset has `required_signatures` with no bypass actors.
  Ordinary `git commit` signs. **Never use `git filter-branch`** — it strips signatures. `git log
  --format=%G?` misreports signed commits here (`gpg.ssh.allowedSignersFile` is unset); verify with
  `git cat-file commit <sha> | grep -q gpgsig`.
- **Commit messages are Conventional Commits**; `scripts/check-commit-messages.sh` must pass.
- **Any `feat`/`fix` PR needs a release-note fragment** in `site/src/content/unreleased/` — this
  branch will need one. `no-release-note` is the escape.
- **Guard possibly-empty array expansions.** Bash 3.2 (the maintainer's `/bin/bash`) aborts on
  `"${arr[@]}"` when empty under `set -u`. Idiom at `scripts/sign-app.sh:30-33`. **Run every
  self-test under `/bin/bash` as well as `bash`.**
- Work on branch `richardcase/copy-claims`. Do not commit to `main`. Do not `git push`.

## Measured facts this plan is built on

Do not re-derive these; they were measured against the real corpus (the five notes in
`site/src/content/releases/`) and they determine the design:

| Word | Appears in |
|---|---|
| `agent` | **5 of 5 notes** |
| `terminal` | 4 of 5 |
| `daemon` | 3 of 5 |
| `pane`, `window` | 2 of 5 |
| `reflow`, `linux`, `survival`, `zero-disruption` | **0 of 5** |

Three consequences:

1. **A stopword list is mandatory.** Issue #55's title contains "agent"; without stopwords it would
   match every note ever written.
2. **Matching must be by prefix, not whole word.** "reflow" scores 0/5 despite v0.7.0 containing
   "now **reflows** the terminal", and "resize" 0/5 against "**Resizing** a window".
3. **The threshold must be one significant word, not two.** After stopwording, #56 ("M8 — Linux
   support") retains only `linux`. A two-word threshold would make it permanently unable to fire.

The gap issues as they stand: **#55 OPEN** "M9c — PTY-host true zero-disruption agent survival",
**#56 OPEN** "M8 — Linux support", **#87 CLOSED** "Terminal grid does not reflow when the window is
resized".

---

### Task 1: `scripts/check-copy-claims.sh`

**Files:**
- Create: `scripts/check-copy-claims.sh`

**Interfaces:**
- Produces: `scripts/check-copy-claims.sh [<base> [<head>]]` exits 0 when no claim is stale and no
  added fragment contradicts an open gap; non-zero otherwise. `--self-test` exits 0 when all cases
  pass. Reads `PR_LABELS` (comma-joined) for the `no-copy-review` escape, exactly as
  `check-release-notes.sh` reads it for `no-release-note`.
- Consumes: `site/src/components/Faq.astro` for `gap:` annotations;
  `site/src/content/unreleased/*.md` for fragments added in the range; the issues API for state.

Study `scripts/check-release-notes.sh` first — this script is its sibling and must match its shape:
`set -euo pipefail`, a `die()`, pure functions above the argument parsing, `--self-test` exercising
those pure functions against fixtures, and comments that explain *why*.

- [ ] **Step 1: Write the pure core, with the API isolated behind one function**

The design constraint that matters: **everything except the API call must be a pure function**, so
`--self-test` can cover the logic without network. Structure it as:

- `parse_gaps <faq-file>` → prints `issue-number` per line, one per `gap:` annotation found.
  Must reject a malformed value (`gap: abc`, `gap:` with nothing) **loudly** rather than skipping it —
  a silently ignored annotation is a claim nobody is watching.
- `significant_words <title>` → lowercases, splits on non-alphanumerics, drops stopwords and words
  shorter than four characters, prints the rest. **Stopword list, from the measurement above:**
  `agent agents terminal terminals daemon daemons pane panes window windows clowder support true
  when the with that this from into your` — plus milestone tokens (`m8`, `m9c`) which
  `check-release-notes.sh` already treats as internal.
- `contradicts <fragment-text> <title>` → 0 if any significant word of the title appears in the
  fragment **as a prefix** (so `reflow` matches `reflows`, `resize` matches `resizing`). One match is
  enough — see the measured facts.
- `issue_state <number>` → the **only** function that touches the network.

- [ ] **Step 2: Write the self-test**

Required cases, each of which must fail against a deliberately broken implementation:

| Case | Expect |
|---|---|
| `gap:` pointing at a closed issue | fail, naming the entry and issue |
| `gap:` pointing at an open issue | pass |
| FAQ entry with no `gap:` | ignored, pass |
| `gap: abc` / empty `gap:` | **fail loudly**, not skipped |
| fragment containing `reflows` vs a title containing `reflow` | fail (prefix match works) |
| fragment containing `Resizing` vs a title containing `resized` | fail |
| fragment mentioning only `agent` vs #55's title | **pass** — stopword suppression |
| fragment sharing no significant word | pass |
| API error from `issue_state` | fail, with a message naming an API error as a possible cause |
| no `gap:` annotations at all | pass, and say so |

Fixtures for the API must be injectable — e.g. an `ISSUE_STATE_CMD` override the self-test points at
a stub — so no case reaches the network.

- [ ] **Step 3: Verify under both shells**

```bash
chmod +x scripts/check-copy-claims.sh
/bin/bash scripts/check-copy-claims.sh --self-test
bash scripts/check-copy-claims.sh --self-test
```

Both must pass, with the same count. `/bin/bash` is 3.2 here and has already caught an empty-array
bug in this repo once.

- [ ] **Step 4: Tune against the real corpus — do not skip this**

Run the contradiction check over **every** note in `site/src/content/releases/` against the current
open gaps (#55, #56), and record the output in your report. Expected: **zero matches** — the
distinctive words (`linux`, `survival`, `zero-disruption`, `pty-host`) appear in none of them.

If anything fires, the stopword list is too narrow; widen it and say what you added and why. A check
that fires on historical notes will fire on every future PR.

- [ ] **Step 5: Prove it would have caught the real miss**

The failure this milestone exists for: issue #87 closed while the FAQ still said scrollback does not
reflow. Reconstruct it — a fixture FAQ entry with `gap: 87` and a stubbed issue state of `closed` —
and confirm the check fails. Put the output in your report. If it passes, the check does not do its
job.

- [ ] **Step 6: Commit**

```bash
git add scripts/check-copy-claims.sh
git commit -m "ci: fail when a site limitation claim goes stale

The FAQ's limitation claims are the copy that drifts: both corrections
needed during M0 and M1 were limitations, while Features.astro's
positive claims needed none. A limitation is true only until someone
fixes the thing, and they are not reading marketing copy.

The falsifier already existed — this repo's issues are written in almost
the FAQ's own words, so a closed issue is the signal. #87 closing is
exactly the event that should have flagged the reflow claim."
```

---

### Task 2: Annotate the FAQ and wire the check into CI

**Files:**
- Modify: `site/src/components/Faq.astro`
- Modify: `.github/workflows/ci.yml` (the `commit-lint` job only)

**Interfaces:**
- Consumes: `scripts/check-copy-claims.sh` from Task 1.

- [ ] **Step 1: Annotate the two limitation entries**

Exactly two of the six FAQ entries assert a limitation. The others make positive or permanent claims
and must be left alone — annotating them would create noise with nothing to watch.

- `q: 'Does it work on Linux or Windows?'` → `gap: 56` (M8 — Linux support)
- `q: 'What happens to my agents when I close the window?'` → `gap: 55` (M9c — PTY-host true
  zero-disruption agent survival). Note this entry was **already corrected once** for going stale;
  what remains true is the residual process-survival gap that #55 tracks.

Add a short comment above the first `gap:` explaining the contract — the claim holds while the issue
is open, and CI fails when it closes so the person who closed it rewrites the answer. Match the
file's existing header voice.

**Do not** annotate `'Is Clowder open source?'`. It is a limitation, but a deliberate business
decision with no issue and no expectation of change; a `gap:` implies something is being worked on.

- [ ] **Step 2: Wire it into `commit-lint`**

After the `Release notes` step, in the same voice as the steps around it:

```yaml
      - name: check-copy-claims.sh self-tests
        run: scripts/check-copy-claims.sh --self-test

      - name: Site copy claims
        env:
          BASE: ${{ github.event.pull_request.base.sha || 'origin/main' }}
          HEAD_REF: ${{ github.event.pull_request.head.sha || github.sha }}
          PR_LABELS: ${{ join(github.event.pull_request.labels.*.name, ',') }}
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: scripts/check-copy-claims.sh "$BASE" "$HEAD_REF"
```

`GH_TOKEN` is required — the issues API on a private repo needs auth. Confirm `commit-lint`'s
`permissions:` block grants `issues: read`, and add it if not; a missing scope surfaces as a 403 that
looks exactly like an API outage.

- [ ] **Step 3: Verify it passes against `main` as it stands**

```bash
scripts/check-copy-claims.sh origin/main HEAD
```

Expected: pass. #55 and #56 are both open, and no fragment on this branch mentions their distinctive
words. If it fails here, the annotation or the stopword list is wrong — not the FAQ.

- [ ] **Step 4: Verify the YAML and step order**

```bash
ruby -ryaml -e 'd=YAML.load_file(".github/workflows/ci.yml"); j=d["jobs"]["commit-lint"]; puts "permissions: #{j["permissions"].inspect}"; j["steps"].each{|s| puts "  - #{s["name"]||s["uses"]}"}'
```

- [ ] **Step 5: Commit**

```bash
git add site/src/components/Faq.astro .github/workflows/ci.yml
git commit -m "ci: annotate the FAQ's limitation claims with their tracking issues

Only the two entries that assert a limitation get a gap: the others make
positive or permanent claims with nothing to watch. 'Is Clowder open
source?' is deliberately excluded — it is a business decision, not a gap
anyone is working to close, and a gap: would imply otherwise."
```

---

### Task 3: Document, and add this branch's release note

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/versioning.md` (one-time setup)
- Create: `site/src/content/unreleased/<slug>.md`

- [ ] **Step 1: Document the convention in `AGENTS.md`**

A Conventions bullet, in the existing voice, covering: a FAQ entry asserting a limitation carries
`gap: <issue>`; CI fails when that issue closes, so the person closing it rewrites the answer; a
release note whose wording overlaps an open gap's title also fails, as a prompt; `no-copy-review` is
the escape and it is reported in the job summary. Say plainly **why it watches limitations and not
`Features.astro`** — positive claims stay true as the product grows, limitation claims do not — so
nobody later "improves" it by extending the gate to all copy.

- [ ] **Step 2: Add the one-time setup to `docs/versioning.md`**

Alongside the existing `release` and `no-release-note` label commands:

```sh
gh label create no-copy-review -c FBCA04 -d "Copy claims reviewed; no change needed"
```

- [ ] **Step 3: Add this branch's release-note fragment**

This branch has `ci:` and `docs:` commits, so the release-note rule may not require one — check with
`scripts/check-release-notes.sh origin/main HEAD`. If it does require one, write it about what a
*user* gains, which is honestly nothing directly: this is internal machinery. In that case take the
`no-release-note` label rather than inventing a user-facing claim, and say so in your report — that
is exactly the case the label exists for.

- [ ] **Step 4: Verify everything documented actually works**

Run every command you documented before you document it. Then:

```bash
/bin/bash scripts/check-copy-claims.sh --self-test
scripts/check-copy-claims.sh origin/main HEAD
scripts/check-release-notes.sh origin/main HEAD
scripts/check-commit-messages.sh
cd site && npm run check && npm test && npm run build && cd ..
```

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md docs/versioning.md site/src/content/unreleased/ 2>/dev/null
git commit -m "docs: document the copy-claims check and its label"
```

---

## Done means

- [ ] `check-copy-claims.sh --self-test` passes under **both** `/bin/bash` 3.2 and `bash` 5.x
- [ ] Every self-test case fails against a deliberately broken implementation (report the mutation evidence)
- [ ] The contradiction check produces **zero** matches across all five existing notes
- [ ] A reconstructed `gap: 87` + closed state **fails** — the real miss would have been caught
- [ ] `scripts/check-copy-claims.sh origin/main HEAD` passes on this branch
- [ ] `commit-lint` has `issues: read` and both new steps
- [ ] `cd site && npm run check && npm test && npm run build` clean
- [ ] `scripts/check-commit-messages.sh` passes; every commit signed

## Not in this plan

**One-time, after merge:** `gh label create no-copy-review -c FBCA04 -d "Copy claims reviewed; no
change needed"`. Until it exists the escape hatch cannot be used, though the check itself works.

**Not attempted:** any gate on `Features.astro`, and any semantic/LLM comparison. Both are argued
against in the spec, on evidence.
