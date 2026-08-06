#!/usr/bin/env bash
# Verify that every non-merge commit on this branch uses Conventional Commits
# (`type(scope): subject`). PRs land on main as merge commits, so branch commits are preserved
# verbatim in main's history — a bad subject is permanent. CI runs this on every PR; run it
# locally before pushing.
#
# Usage: scripts/check-commit-messages.sh [<base-ref> [<head-ref>]]   (default: origin/main HEAD)
#        scripts/check-commit-messages.sh --check-subject "<subject>" (validate one literal subject)
set -euo pipefail

# Scope is free-form on purpose: milestone scopes (`m10c`) and comma-joined scopes (`proto,daemon`)
# are both already in use. A trailing `!` marks a breaking change.
TYPES='feat|fix|docs|test|refactor|perf|ci|chore|build|style|revert'
PATTERN="^(${TYPES})(\([^)]+\))?!?: .+"

usage() {
  cat <<'EOF'
Usage: scripts/check-commit-messages.sh [<base-ref> [<head-ref>]]   (default: origin/main HEAD)
       scripts/check-commit-messages.sh --check-subject "<subject>" (validate one literal subject)
EOF
}

# Returns 0 if the subject conforms. Reverts produced by GitHub's revert button (`Revert "…"`) are
# exempt — that subject is not ours to shape. `fixup!`/`squash!` are deliberately NOT exempt: an
# unsquashed fixup would land in main forever.
subject_ok() {
  case "$1" in
    'Revert "'*) return 0 ;;
  esac
  printf '%s\n' "$1" | grep -Eq "$PATTERN"
}

explain() {
  cat >&2 <<EOF

Commit subjects must follow Conventional Commits:

    type(scope): subject
    type(scope)!: subject     # breaking change

  type     one of: ${TYPES//|/, }
  scope    optional, free-form, in parentheses
  subject  non-empty, after ": "

Examples: feat(daemon): route attention events
          fix(app): keep the sidebar selection after a restart
          docs: document the commit convention

Reword with 'git commit --amend' (last commit) or 'git rebase -i <base>' (older ones).
For 'fixup!'/'squash!' commits, fold them in with 'git rebase -i --autosquash <base>'.
EOF
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
  --check-subject)
    if [ "$#" -ne 2 ]; then
      echo "error: --check-subject takes exactly one argument" >&2
      exit 2
    fi
    if subject_ok "$2"; then exit 0; else exit 1; fi
    ;;
esac

if [ "$#" -gt 2 ]; then
  usage >&2
  exit 2
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BASE="${1:-origin/main}"
HEAD_REF="${2:-HEAD}"

for ref in "$BASE" "$HEAD_REF"; do
  if ! git rev-parse --verify --quiet "${ref}^{commit}" >/dev/null; then
    echo "error: '$ref' does not resolve to a commit in this checkout" >&2
    echo "hint: a shallow clone may not contain it — 'git fetch --unshallow' (CI uses fetch-depth: 0)" >&2
    exit 2
  fi
done

if ! merge_base="$(git merge-base "$BASE" "$HEAD_REF" 2>/dev/null)"; then
  echo "error: '$BASE' and '$HEAD_REF' have no common ancestor" >&2
  exit 2
fi

# --no-merges drops "Merge pull request #N …" and 'Update branch' merges structurally.
total=0
bad=""
bad_count=0
while IFS= read -r sha; do
  [ -n "$sha" ] || continue
  total=$((total + 1))
  subject="$(git log -1 --format=%s "$sha")"
  if ! subject_ok "$subject"; then
    bad="${bad}    $(git rev-parse --short "$sha") ${subject}"$'\n'
    bad_count=$((bad_count + 1))
  fi
done < <(git rev-list --no-merges "$merge_base..$HEAD_REF")

if [ "$bad_count" -gt 0 ]; then
  echo "✗ $bad_count of $total commit(s) in $(git rev-parse --short "$merge_base")..$HEAD_REF do not match Conventional Commits:" >&2
  printf '%s' "$bad" >&2
  explain
  exit 1
fi

echo "✓ $total commit(s) match Conventional Commits ($(git rev-parse --short "$merge_base")..$HEAD_REF)"
