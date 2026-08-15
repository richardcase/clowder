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
#
# Restricted to *.md: the pathspec is the fragment directory's markdown files only, not every added
# file in it. A later task adds a `.gitkeep` to keep the (currently nonexistent) directory in git —
# without this restriction, that placeholder would silently satisfy the note requirement, both on
# this branch and on any future PR that recreates `.gitkeep` after a release empties the directory.
added_fragments() {
  git diff --name-only --diff-filter=A "$1" "$2" -- "$FRAGMENT_DIR/*.md" || true
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
