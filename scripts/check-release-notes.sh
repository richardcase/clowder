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

# Patterns that must never reach a public page, split into two groups that need DIFFERENT case
# handling — see the case-insensitive/-sensitive split in guard_file below:
#
#   - FORBIDDEN_LINK: the private repo under either owner (the org moved; both forms exist in old
#     release bodies). Case-INsensitive: a URL is a URL whether or not a client-side link-checker
#     or a human typo'd the host/owner casing (`GitHub.com`, `DefiantSoftware`) — there is no
#     legitimate reason for case to matter here.
#     Boundary: `github\.com/defiantsoftware/clowder` must be rejected as a bare mention (end of
#     string), inside a markdown link `[text](url)` (next char `)`), and followed by ordinary
#     prose punctuation (`.`, `,`) or whitespace — but NOT when it is a prefix of a longer,
#     legitimate repo name (`clowder-site`, and by construction `homebrew-clowder` never matches
#     the literal at all since "clowder" there isn't preceded by "defiantsoftware/"). A negated
#     word-or-hyphen class captures exactly that: reject unless the next char continues a
#     path/word segment. This replaces an earlier `([/"?#]|$)` boundary that was copied from
#     site/scripts/audit.sh, which scans *built HTML* where a link is always followed by `"` —
#     right for that medium, wrong for markdown prose, where the terminators are `)`, `.`, `,`,
#     and whitespace. The old owner (`richardcase`) is deliberately left with NO boundary: that
#     org has no legitimate public repos left to false-positive on, so any mention at all — with
#     any suffix — is suspicious.
#   - FORBIDDEN_OTHER: `#123` PR/issue references (which resolve to nothing public) and internal
#     milestone scopes (m7d, m10c, m11a, m12b). Case-SENSITIVE, and deliberately not merged into
#     FORBIDDEN_LINK's case-insensitive pass: `\bm[0-9]+[a-z]?\b` matching case-insensitively would
#     flag "Apple M1" (legitimate copy for a Mac app) as a milestone scope. The `#[0-9]+` here is
#     bounded to 1-5 digits not immediately followed by another digit, so a plausible PR/issue
#     number like `#82` or `#123` still matches but an all-numeric 6-digit hex colour like
#     `#123456` — plausible in a terminal-app theming note — does not. This is a heuristic, not a
#     guarantee: nothing distinguishes a genuine 6-digit issue number from a hex colour by pattern
#     alone, and this repo is nowhere near 100,000 PRs. Accepted trade-off, not a design to defend
#     past this repo's scale.
FORBIDDEN_LINK='github\.com/richardcase/clowder|github\.com/defiantsoftware/clowder([^A-Za-z0-9_-]|$)'
FORBIDDEN_OTHER='#[0-9]{1,5}([^0-9]|$)|\bm[0-9]+[a-z]?\b'

die() { echo "error: $*" >&2; exit 2; }

# guard_file <path> -> 0 clean, 1 violation
guard_file() {
  local f="$1" hits
  [ -f "$f" ] || die "no such file: $f"
  hits="$(
    { grep -nIoiE "$FORBIDDEN_LINK" "$f" || true
      grep -nIoE "$FORBIDDEN_OTHER" "$f" || true
    } | sort -t: -k1,1n
  )"
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
#
# Restricted to *.md: the pathspec is the fragment directory's markdown files only, not every added
# file in it. A later task adds a `.gitkeep` to keep the (currently nonexistent) directory in git —
# without this restriction, that placeholder would silently satisfy the note requirement, both on
# this branch and on any future PR that recreates `.gitkeep` after a release empties the directory.
added_fragments() {
  git diff --name-only --diff-filter=A "$1" "$2" -- "$FRAGMENT_DIR/*.md" || true
}

# touched_files <base> <head> -> prints paths added, modified, or renamed into either guarded dir
#
# --no-renames + --diff-filter=AMR: without --no-renames, git's default rename detection can
# classify a file moved WITHIN this combined pathspec (e.g. an old public releases/*.md renamed
# into unreleased/, then rewritten) as type R — which a plain `--diff-filter=AM` excludes, so the
# guard below never runs on it, even though `added_fragments`'s narrower pathspec (source path
# excluded) still reports the same change as a plain add and lets it satisfy the note requirement.
# --no-renames forces git to report the change as a delete of the source plus an add of the
# destination instead, so the destination is scanned like any other add. `--diff-filter=AMR` keeps
# R in the filter as a belt-and-suspenders: --no-renames should make R impossible to emit here, but
# if some caller's git config ever overrode that, R staying in the allowed set means the change is
# still caught rather than silently excluded. See self_test's rename fixture, which reproduces this
# exact evasion in a scratch repo and asserts the destination is both reported and guarded.
touched_files() {
  git diff --no-renames --name-only --diff-filter=AMR "$1" "$2" \
    -- "$FRAGMENT_DIR" 'site/src/content/releases' || true
}

self_test() {
  local pass=0 fail=0 tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  check_guard() {
    local want="$1" name="$2" body="$3" got
    local f="$tmp/$name.md"
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

  # Case-insensitivity on the link patterns only (fix round 1, Finding 2). Verified against the
  # unfixed script before this fix landed: both passed clean.
  check_guard violation link-case-domain 'https://GitHub.com/defiantsoftware/clowder/issues/1'
  check_guard violation link-case-owner  'https://github.com/DefiantSoftware/Clowder/issues/1'

  # Boundary widening (fix round 1, Finding 3) — markdown's actual terminators, not built-HTML's.
  # Verified against the unfixed script before this fix landed: all three passed clean.
  check_guard violation markdown-link  'See [the repo](https://github.com/defiantsoftware/clowder).'
  check_guard violation trailing-period 'Read more at https://github.com/defiantsoftware/clowder.'
  check_guard violation trailing-comma  'Source: https://github.com/defiantsoftware/clowder, more soon.'

  # Must be accepted — the public repos share a prefix with the private one, and ordinary prose
  # about the product must not trip the guard.
  check_guard clean tap-link       'Install with the tap at https://github.com/defiantsoftware/homebrew-clowder'
  check_guard clean site-link      'https://github.com/defiantsoftware/clowder-site is the old site repo'
  check_guard clean plain-prose    'Connect the app to a Clowder daemon on another machine over TLS.'
  check_guard clean version-number 'Requires macOS 14 or later.'
  # A markdown heading is `#` followed by a space, not digits — must not trip the `#[0-9]+` rule.
  # (The `#123` fixture above is what proves that rule fires at all; this one proves it does not
  # over-fire on ordinary markdown.)
  check_guard clean markdown-heading '## What changed'
  # A 6-digit all-numeric hex colour must not be mistaken for a 6+-digit PR/issue reference (fix
  # round 1, minor finding). Verified against the unfixed script before this fix landed: flagged
  # as a violation.
  check_guard clean hex-color-six-digit 'The accent colour is #123456.'
  # Locks in the Finding-2 boundary: the milestone-token pattern stays case-SENSITIVE on purpose,
  # so ordinary capitalized copy like "Apple M1" must never trip it. Already passed before this
  # fix round (case-sensitivity here was never broken) — kept as regression coverage so a future
  # "just add -i everywhere" edit gets caught here instead of in a real release body.
  check_guard clean apple-m1-not-milestone 'The new Apple M1 chip is fast.'

  echo
  echo "check-release-notes: verifying a same-pathspec rename cannot evade the guard (Finding 1)"
  # Reproduces the exact evasion: an old, clean public release note renamed from releases/ into
  # unreleased/ and rewritten with private-repo content in the same commit. Without --no-renames,
  # git's default rename detection classifies this as a single R-typed change (confirmed: git
  # reports it as a rename when the rewritten body keeps enough of the original text — a bare
  # rewrite with no shared content falls back to plain delete+add and never exercised the bug).
  # Kept inside $tmp so the existing `trap ... RETURN` above cleans it up too.
  local rename_repo="$tmp/rename-repo" rbase rhead touched_out
  mkdir -p "$rename_repo/site/src/content/releases" "$rename_repo/site/src/content/unreleased"
  (
    cd "$rename_repo"
    git init -q
    git config user.email test@test.com
    git config user.name Test
    git config commit.gpgsign false
    cat > site/src/content/releases/v1.md <<'SEED'
Connect the app to a Clowder daemon on another machine over TLS.
This release adds host pairing, a settings panel for managing hosts,
and a connection status indicator in the sidebar so you always know
which backend you are talking to. Existing local workflows are
unaffected by this change and continue to work exactly as before.
SEED
    git add -A
    git commit -q -m "chore: seed"
    git rev-parse HEAD > "$rename_repo/.base-sha"

    git mv site/src/content/releases/v1.md site/src/content/unreleased/leak.md
    cat > site/src/content/unreleased/leak.md <<'REWRITE'
Connect the app to a Clowder daemon on another machine over TLS.
This release adds host pairing, a settings panel for managing hosts,
and a connection status indicator in the sidebar so you always know
which backend you are talking to. See https://github.com/richardcase/clowder/pull/72
and m11a (#82) for the implementation.
REWRITE
    git add -A
    git commit -q -m "chore: rename and rewrite"
    git rev-parse HEAD > "$rename_repo/.head-sha"
  )
  rbase="$(cat "$rename_repo/.base-sha")"
  rhead="$(cat "$rename_repo/.head-sha")"
  # Sanity check on the fixture itself: confirm git actually classified this as a rename (R),
  # which is the precondition for the evasion existing at all. If this ever stops being true (a
  # future git default change, say), the two assertions below would pass vacuously.
  if ! (cd "$rename_repo" && git diff --name-status "$rbase" "$rhead") | grep -q '^R'; then
    echo "  FAIL  rename-fixture-is-actually-a-rename — git did not classify the seed change as R; this fixture no longer exercises Finding 1" >&2
    fail=$((fail + 1))
  else
    echo "  ok    rename-fixture-is-actually-a-rename"
    pass=$((pass + 1))
  fi
  touched_out="$(cd "$rename_repo" && touched_files "$rbase" "$rhead")"
  if printf '%s\n' "$touched_out" | grep -qx 'site/src/content/unreleased/leak.md'; then
    echo "  ok    rename-destination-is-reported-as-touched"
    pass=$((pass + 1))
  else
    echo "  FAIL  rename-destination-is-reported-as-touched — touched_files did not report the rename's destination, so the guard would never run on it" >&2
    fail=$((fail + 1))
  fi
  if (cd "$rename_repo" && guard_file site/src/content/unreleased/leak.md) >/dev/null 2>&1; then
    echo "  FAIL  rename-destination-is-guarded — guard_file passed content that contains a private-repo link and a milestone scope" >&2
    fail=$((fail + 1))
  else
    echo "  ok    rename-destination-is-guarded (violation)"
    pass=$((pass + 1))
  fi

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
touched="$(touched_files "$merge_base" "$HEAD_REF")"
guard_status=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  # Guard markdown content only — a `.gitkeep` (or any other non-.md file) added to keep the
  # directory in git carries no prose and must not be scanned for it.
  case "$f" in
    *.md) ;;
    *) continue ;;
  esac
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
