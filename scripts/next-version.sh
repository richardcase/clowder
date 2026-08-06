#!/usr/bin/env bash
# Derive the next release version from Conventional Commits since the last release tag.
#
# Prints `key=value` lines on stdout — append them straight to $GITHUB_OUTPUT. A human-readable
# breakdown goes to stderr. "Nothing to release" is `release=false` with exit 0, NOT an error: a
# dispatch that finds only docs/chore commits should end green, having done nothing.
#
# Usage:
#   scripts/next-version.sh [--prerelease <id>] [--override <X.Y.Z[-id]>] [--ref <rev>]
#   scripts/next-version.sh --notes [--ref <rev>]   # markdown changelog for the same range
#   scripts/next-version.sh --self-test             # table-driven unit tests, no git access
#
# Bump rules (see docs/versioning.md):
#   feat                    -> minor
#   fix | perf              -> patch
#   `!` / BREAKING CHANGE:  -> major, EXCEPT while major == 0, where it bumps MINOR (0.4.0 -> 0.5.0)
#   docs|test|refactor|ci|chore|build|style|revert alone -> nothing to release (exit 0)
# Non-releasable commits still appear in --notes and in the GitHub-generated release notes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/conventional.sh
. "$ROOT/scripts/lib/conventional.sh"

# Strict semver: X.Y.Z, no leading zeros, optional -prerelease. Deliberately stricter than
# set-version.sh's historical glob, which accepted '01.2.3' and '1.2.3junk'.
SEMVER_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'

die() { echo "error: $*" >&2; exit 2; }

usage() {
  cat <<'EOF'
Usage: scripts/next-version.sh [--prerelease <id>] [--override <X.Y.Z[-id]>] [--ref <rev>]
       scripts/next-version.sh --notes [--ref <rev>]
       scripts/next-version.sh --self-test
EOF
}

# ------------------------------------------------------------------ pure helpers
# Everything in this section is exercised by --self-test and touches no git state.

# fold_bump <X.Y.Z> <major|minor|patch> -> the bump actually applied at that version.
#
# 0.x: the public API is unstable by definition, so a breaking change is a MINOR bump. Promoting to
# 1.0.0 is a deliberate act, not something a commit marker triggers — use --override.
#
# Separate from bump_version so callers can REPORT the effective bump: saying "major" while
# producing 0.4.0 -> 0.5.0 would be a lie in the step summary and the release PR body.
fold_bump() {
  local v="$1" kind="$2"
  [[ $v =~ $SEMVER_RE ]] || die "'$v' is not semver X.Y.Z"
  if [ "$kind" = major ] && [ "${BASH_REMATCH[1]}" -eq 0 ]; then echo minor; else echo "$kind"; fi
}

# bump_version <X.Y.Z> <major|minor|patch> -> next X.Y.Z
bump_version() {
  local v="$1" kind major minor patch
  kind="$(fold_bump "$1" "$2")"
  [[ $v =~ $SEMVER_RE ]] || die "'$v' is not semver X.Y.Z"
  major="${BASH_REMATCH[1]}"
  minor="${BASH_REMATCH[2]}"
  patch="${BASH_REMATCH[3]}"
  case "$kind" in
    major) printf '%s.0.0\n'   "$((major + 1))" ;;
    minor) printf '%s.%s.0\n'  "$major" "$((minor + 1))" ;;
    patch) printf '%s.%s.%s\n' "$major" "$minor" "$((patch + 1))" ;;
    *) die "unknown bump kind '$kind'" ;;
  esac
}

# bump_for_type <type> <breaking 0|1> -> major|minor|patch|none
bump_for_type() {
  if [ "$2" = 1 ]; then echo major; return 0; fi
  case "$1" in
    feat)     echo minor ;;
    fix|perf) echo patch ;;
    *)        echo none ;;
  esac
}

# rank <none|patch|minor|major> -> 0..3, so a scan can keep the largest bump it has seen.
rank() {
  case "$1" in
    major) echo 3 ;;
    minor) echo 2 ;;
    patch) echo 1 ;;
    *)     echo 0 ;;
  esac
}

# ------------------------------------------------------------------- self-test
self_test() {
  local fails=0
  check() {
    if [ "$2" = "$3" ]; then
      printf '  ok    %-40s %s\n' "$1" "$3"
    else
      printf '  FAIL  %-40s expected %s, got %s\n' "$1" "$2" "$3"
      fails=$((fails + 1))
    fi
  }
  # Asserts that cc_parse REJECTS a subject (used for the negative cases below).
  refutes() {
    if cc_parse "$2"; then check "$1" reject accept; else check "$1" reject reject; fi
  }

  echo "bump_version (0.x rule):"
  check "0.4.0 + major -> minor" 0.5.0  "$(bump_version 0.4.0 major)"
  check "0.0.5 + major -> minor" 0.1.0  "$(bump_version 0.0.5 major)"
  check "0.4.0 + minor"          0.5.0  "$(bump_version 0.4.0 minor)"
  check "0.4.0 + patch"          0.4.1  "$(bump_version 0.4.0 patch)"
  check "0.9.9 + minor"          0.10.0 "$(bump_version 0.9.9 minor)"
  echo "bump_version (>=1.0.0):"
  check "1.0.0 + major"          2.0.0  "$(bump_version 1.0.0 major)"
  check "1.2.3 + minor"          1.3.0  "$(bump_version 1.2.3 minor)"
  check "1.2.3 + patch"          1.2.4  "$(bump_version 1.2.3 patch)"
  check "1.9.0 + major"          2.0.0  "$(bump_version 1.9.0 major)"

  # The reported bump must equal the applied one, or the step summary and PR body lie.
  echo "fold_bump (what gets reported):"
  check "0.4.0 major -> minor" minor "$(fold_bump 0.4.0 major)"
  check "0.4.0 minor unchanged" minor "$(fold_bump 0.4.0 minor)"
  check "0.4.0 patch unchanged" patch "$(fold_bump 0.4.0 patch)"
  check "1.0.0 major unchanged" major "$(fold_bump 1.0.0 major)"
  check "fold agrees with bump_version" \
    "0.5.0|minor" "$(bump_version 0.4.0 major)|$(fold_bump 0.4.0 major)"

  echo "bump_for_type:"
  check "feat"      minor "$(bump_for_type feat 0)"
  check "fix"       patch "$(bump_for_type fix 0)"
  check "perf"      patch "$(bump_for_type perf 0)"
  check "docs"      none  "$(bump_for_type docs 0)"
  check "chore"     none  "$(bump_for_type chore 0)"
  check "refactor"  none  "$(bump_for_type refactor 0)"
  check "feat!"     major "$(bump_for_type feat 1)"
  check "refactor!" major "$(bump_for_type refactor 1)"

  echo "rank ordering:"
  check "major > minor" greater "$([ "$(rank major)" -gt "$(rank minor)" ] && echo greater || echo no)"
  check "minor > patch" greater "$([ "$(rank minor)" -gt "$(rank patch)" ] && echo greater || echo no)"
  check "patch > none"  greater "$([ "$(rank patch)" -gt "$(rank none)"  ] && echo greater || echo no)"

  echo "cc_parse:"
  cc_parse 'feat(daemon)!: provision worktrees outside the project'
  check "feat(daemon)!"     "feat|daemon|1"           "$CC_TYPE|$CC_SCOPE|$CC_BREAKING"
  cc_parse 'fix: keep the sidebar selection'
  check "fix, no scope"     "fix||0"                  "$CC_TYPE|$CC_SCOPE|$CC_BREAKING"
  cc_parse 'docs(workspace,config): correct two messages'
  check "comma scope"       "docs|workspace,config|0" "$CC_TYPE|$CC_SCOPE|$CC_BREAKING"
  cc_parse 'feat!: no scope but breaking'
  check "feat! no scope"    "feat||1"                 "$CC_TYPE|$CC_SCOPE|$CC_BREAKING"
  cc_parse 'docs(spec): design for worktrees (#65)'
  check "trailing (#NN)"    "docs|spec|0"             "$CC_TYPE|$CC_SCOPE|$CC_BREAKING"

  refutes "reject wip:"          'wip: nope'
  refutes "reject merge commit"  'Merge pull request #68 from richardcase/x'
  refutes "reject missing colon" 'feat no colon'
  refutes "reject empty desc"    'feat: '
  # A revert is VALID but carries no type, so it contributes no bump — both halves matter.
  if cc_subject_ok 'Revert "feat: x"'; then check "Revert is valid" ok ok; else check "Revert is valid" ok bad; fi
  refutes "Revert yields no type" 'Revert "feat: x"'

  echo
  if [ "$fails" -gt 0 ]; then
    echo "✗ $fails self-test failure(s)" >&2
    return 1
  fi
  echo "✓ next-version.sh self-tests pass"
}

# ------------------------------------------------------------------------ args
MODE=version
PRERELEASE=''
OVERRIDE=''
REF='HEAD'
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prerelease) [ "$#" -ge 2 ] || die "--prerelease needs a value"; PRERELEASE="$2"; shift 2 ;;
    --override)   [ "$#" -ge 2 ] || die "--override needs a value";   OVERRIDE="$2";   shift 2 ;;
    --ref)        [ "$#" -ge 2 ] || die "--ref needs a value";        REF="$2";        shift 2 ;;
    --notes)      MODE=notes;    shift ;;
    --self-test)  MODE=selftest; shift ;;
    -h|--help)    usage; exit 0 ;;
    *)            die "unknown argument '$1' (try --help)" ;;
  esac
done

if [ "$MODE" = selftest ]; then self_test; exit $?; fi

cd "$ROOT"
git rev-parse --verify --quiet "${REF}^{commit}" >/dev/null \
  || die "'$REF' does not resolve to a commit (a shallow clone needs 'git fetch --unshallow'; CI uses fetch-depth: 0)"

# The previous release is the nearest ANCESTOR FINAL tag. Pre-releases are excluded on purpose: a
# run after v0.5.0-rc1 must still measure from v0.4.0 so it lands on v0.5.0, not v0.5.1.
PREV_TAG="$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' --exclude '*-*' "$REF" 2>/dev/null || true)"
CURRENT="$(tr -d '[:space:]' < VERSION)"

if [ -n "$PREV_TAG" ]; then
  PREV_VERSION="${PREV_TAG#v}"
  RANGE="$PREV_TAG..$REF"
else
  # Bootstrap: no release tag exists yet. Measure all of history and bump from the VERSION file.
  PREV_VERSION="$CURRENT"
  RANGE="$REF"
  echo "warning: no v* release tag reachable from $REF — bootstrapping from VERSION ($CURRENT)" >&2
fi

if [ "$MODE" = notes ]; then
  emit() { # emit <heading> <type alternation>
    local body
    body="$(git log --no-merges --reverse --format='%s' "$RANGE" \
            | grep -E "^($2)(\([^)]+\))?!?: " || true)"
    [ -n "$body" ] || return 0
    printf '### %s\n\n' "$1"
    printf '%s\n' "$body" | sed -E 's/^[a-z]+(\([^)]*\))?(!)?: /- /'
    printf '\n'
  }
  emit 'Features'    'feat'
  emit 'Fixes'       'fix'
  emit 'Performance' 'perf'
  emit 'Other'       'docs|test|refactor|ci|chore|build|style|revert'
  exit 0
fi

# --no-merges is mandatory: every PR lands as "Merge pull request #N from …", which is not a
# conventional subject and carries no bump information.
best=none
total=0
releasable=0
unparsed=0
detail=''
while IFS= read -r subject; do
  [ -n "$subject" ] || continue
  total=$((total + 1))
  if ! cc_parse "$subject"; then
    unparsed=$((unparsed + 1))
    detail="${detail}$(printf '  %-6s  %s' '--' "$subject")"$'\n'
    continue
  fi
  kind="$(bump_for_type "$CC_TYPE" "$CC_BREAKING")"
  [ "$kind" = none ] || releasable=$((releasable + 1))
  if [ "$(rank "$kind")" -gt "$(rank "$best")" ]; then best="$kind"; fi
  detail="${detail}$(printf '  %-6s  %s' "$kind" "$subject")"$'\n'
done < <(git log --no-merges --format='%s' "$RANGE")

# A `BREAKING CHANGE:` / `BREAKING-CHANGE:` footer is equivalent to `!`. This repo has none in 300+
# commits, but the spec allows it and ignoring it would silently under-bump.
if git log --no-merges --format='%b' "$RANGE" | grep -Eq '^BREAKING[ -]CHANGE:'; then
  best=major
  releasable=$((releasable + 1))
fi

if [ -n "$OVERRIDE" ]; then
  [[ $OVERRIDE =~ $SEMVER_RE ]] || die "--override '$OVERRIDE' is not semver X.Y.Z[-id]"
  NEXT="$OVERRIDE"
  BUMP=override
  RELEASE=true
elif [ "$best" = none ]; then
  NEXT=''
  BUMP=none
  RELEASE=false
else
  NEXT="$(bump_version "$PREV_VERSION" "$best")"
  # Report what was APPLIED, not what the commits asked for: on 0.x a `!` asks for major but yields
  # a minor bump. `bump_raw` keeps the request visible so the 0.x fold is auditable.
  BUMP="$(fold_bump "$PREV_VERSION" "$best")"
  RELEASE=true
fi
BUMP_RAW="$best"

if [ "$RELEASE" = true ] && [ -n "$PRERELEASE" ]; then
  case "$PRERELEASE" in
    *[!0-9A-Za-z.-]*) die "prerelease id '$PRERELEASE' must match [0-9A-Za-z.-]+" ;;
  esac
  case "$NEXT" in
    *-*) die "'$NEXT' already carries a pre-release id — drop --prerelease" ;;
  esac
  NEXT="$NEXT-$PRERELEASE"
fi

IS_PRE=false
case "$NEXT" in *-*) IS_PRE=true ;; esac

# false when a previous run already bumped VERSION but failed before publishing — that is the
# recovery path: skip the bump PR and go straight to build + tag.
NEEDS_BUMP=false
if [ "$RELEASE" = true ] && [ "$NEXT" != "$CURRENT" ]; then NEEDS_BUMP=true; fi

{
  echo "previous release : ${PREV_TAG:-<none>}"
  echo "range            : $RANGE"
  echo "VERSION file     : $CURRENT"
  echo "commits          : $total ($releasable releasable, $unparsed unparsed)"
  printf '%s' "$detail"
  if [ "$BUMP" != "$BUMP_RAW" ]; then
    echo "bump             : $BUMP (commits asked for $BUMP_RAW; folded by the 0.x rule)"
  else
    echo "bump             : $BUMP"
  fi
  echo "next             : ${NEXT:-<nothing to release>}"
} >&2

cat <<EOF
prev_tag=$PREV_TAG
prev_version=$PREV_VERSION
current_version=$CURRENT
range=$RANGE
commits=$total
releasable=$releasable
unparsed=$unparsed
bump=$BUMP
bump_raw=$BUMP_RAW
release=$RELEASE
needs_bump=$NEEDS_BUMP
version=$NEXT
tag=${NEXT:+v$NEXT}
prerelease=$IS_PRE
EOF
