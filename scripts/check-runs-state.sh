#!/usr/bin/env bash
# Classify a commit's check runs against a set of required check names.
#
# Prints one TSV line per required name: `name<TAB>state<TAB>conclusion<TAB>url`, where state is
# one of `missing` (no check run reported at all), `pending` (queued/in_progress/…), `passed`
# (completed/success) or `failed` (completed, anything else). Exit status is 0 whenever the
# classification succeeded — the CALLER decides what to do about the states.
#
# Usage:
#   scripts/check-runs-state.sh --sha <sha> [--repo owner/name] [--required <json array>]
#   scripts/check-runs-state.sh --from-json <file> --required <json array>
#   scripts/check-runs-state.sh --self-test
#
# `--sha` fetches live via `gh`; `--from-json` classifies a saved
# `GET /repos/{repo}/commits/{sha}/check-runs` body. Both go through the same code, which is the
# point: the classification is what release.yml depends on, so it is testable without a release.
#
# Why this is a script and not inline jq in the workflow: the release pipeline's first real run was
# broken by a check-wait bug, and a second bug (counting occurrences of the required names rather
# than distinct names, so two copies of one check satisfied a `>= 2` gate while the other was
# absent) shipped undetected because none of it could execute outside a real release. The decision
# is a pure function of JSON; --self-test pins it.
set -euo pipefail

die() { echo "error: $*" >&2; exit 2; }

usage() {
  cat <<'EOF'
Usage: scripts/check-runs-state.sh --sha <sha> [--repo owner/name] [--required <json array>]
       scripts/check-runs-state.sh --from-json <file> --required <json array>
       scripts/check-runs-state.sh --self-test
EOF
}

# ------------------------------------------------------------------ classification (pure)
# classify <check-runs json> <required names json array> -> TSV on stdout
#
# A required name can carry MORE THAN ONE check run on the same SHA: the release workflow's
# `workflow_dispatch` run and the PR's own `pull_request` run both report (verified — 4 check runs
# across 2 names on the v0.5.0 bump PR), and a re-run adds another. GitHub evaluates the LATEST per
# name, so this does too: `started_at` first (GitHub's own ordering), `id` as a monotonic tie-break.
#
# Note `?filter=latest` on the API does NOT do this — it de-duplicates within a check *suite*, not
# across suites, and still returns all four.
classify() {
  jq -r --argjson required "$2" '
    [.check_runs[]?] as $runs
    | $required[] as $name
    | ($runs | map(select(.name == $name)) | max_by([(.started_at // ""), (.id // 0)])) as $r
    | [ $name,
        (if $r == null then "missing"
         elif $r.status != "completed" then "pending"
         elif $r.conclusion == "success" then "passed"
         else "failed" end),
        ($r.conclusion // ""),
        ($r.html_url // "") ]
    | @tsv' <<<"$1"
}

# The required contexts as declared by the branch ruleset — not a literal in this file, so adding a
# third required check gates the release automatically. Plain repo read; no `checks` scope needed.
required_from_ruleset() {
  gh api "repos/$1/rules/branches/$2" \
    --jq '[.[] | select(.type == "required_status_checks")
               | .parameters.required_status_checks[].context] | unique'
}

# ------------------------------------------------------------------------- self-test
self_test() {
  local fails=0
  check() {
    if [ "$2" = "$3" ]; then
      printf '  ok    %-46s %s\n' "$1" "$3"
    else
      printf '  FAIL  %-46s expected %s, got %s\n' "$1" "$2" "$3"
      fails=$((fails + 1))
    fi
  }
  # states <json> -> "nameA=state,nameB=state" for the two-name required set
  local req='["A","B"]'
  states() { classify "$1" "$req" | awk -F'\t' '{printf "%s%s=%s", (NR>1 ? "," : ""), $1, $2}'; }

  check "no check runs at all" "A=missing,B=missing" \
    "$(states '{"check_runs":[]}')"

  # THE BUG THAT SHIPPED: the old gate counted occurrences of the required names, so two copies of
  # one name satisfied `length >= 2` while the other name had never reported.
  check "one name twice, other absent" "A=passed,B=missing" \
    "$(states '{"check_runs":[
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","id":1},
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:01:00Z","id":2}]}')"

  check "latest wins: success then running" "A=pending,B=missing" \
    "$(states '{"check_runs":[
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","id":1},
        {"name":"A","status":"in_progress","conclusion":null,"started_at":"2026-01-01T00:05:00Z","id":2}]}')"

  check "latest wins: failure then re-run success" "A=passed,B=missing" \
    "$(states '{"check_runs":[
        {"name":"A","status":"completed","conclusion":"failure","started_at":"2026-01-01T00:00:00Z","id":1},
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:05:00Z","id":2}]}')"

  # During the 2026-08-06 Actions incident a job was cancelled with no runner ever assigned.
  check "cancelled is a failure" "A=failed,B=missing" \
    "$(states '{"check_runs":[{"name":"A","status":"completed","conclusion":"cancelled","started_at":"2026-01-01T00:00:00Z","id":1}]}')"

  # GitHub's rulesets count skipped/neutral as PASSING. We are deliberately stricter: a release must
  # not ship on a check that did not execute. `commit messages` really does report skipped on push.
  check "skipped is a failure (stricter than GH)" "A=failed,B=missing" \
    "$(states '{"check_runs":[{"name":"A","status":"completed","conclusion":"skipped","started_at":"2026-01-01T00:00:00Z","id":1}]}')"
  check "neutral is a failure (stricter than GH)" "A=failed,B=missing" \
    "$(states '{"check_runs":[{"name":"A","status":"completed","conclusion":"neutral","started_at":"2026-01-01T00:00:00Z","id":1}]}')"

  check "null started_at does not crash" "A=passed,B=missing" \
    "$(states '{"check_runs":[{"name":"A","status":"completed","conclusion":"success","started_at":null,"id":7}]}')"

  check "queued is pending" "A=pending,B=missing" \
    "$(states '{"check_runs":[{"name":"A","status":"queued","conclusion":null,"started_at":"2026-01-01T00:00:00Z","id":1}]}')"

  check "both green" "A=passed,B=passed" \
    "$(states '{"check_runs":[
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","id":1},
        {"name":"B","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","id":2}]}')"

  # Unrelated check runs (Copilot review, third-party apps) must not affect the required set.
  check "ignores non-required check runs" "A=passed,B=missing" \
    "$(states '{"check_runs":[
        {"name":"A","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","id":1},
        {"name":"copilot-pull-request-reviewer","status":"completed","conclusion":"failure","started_at":"2026-01-01T00:00:00Z","id":9}]}')"

  # Ordering must follow the required list, so callers can rely on it.
  check "output order follows the required list" "A B" \
    "$(classify '{"check_runs":[]}' '["A","B"]' | cut -f1 | tr '\n' ' ' | sed 's/ $//')"

  echo
  if [ "$fails" -gt 0 ]; then
    echo "✗ $fails self-test failure(s)" >&2
    return 1
  fi
  echo "✓ check-runs-state.sh self-tests pass"
}

# ----------------------------------------------------------------------------- args
MODE=''
SHA=''
JSON_FILE=''
REPO="${GITHUB_REPOSITORY:-}"
BRANCH_FOR_RULES='main'
REQUIRED=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --sha)       [ "$#" -ge 2 ] || die "--sha needs a value";       MODE=sha; SHA="$2"; shift 2 ;;
    --from-json) [ "$#" -ge 2 ] || die "--from-json needs a value"; MODE=json; JSON_FILE="$2"; shift 2 ;;
    --repo)      [ "$#" -ge 2 ] || die "--repo needs a value";      REPO="$2"; shift 2 ;;
    --required)  [ "$#" -ge 2 ] || die "--required needs a value";  REQUIRED="$2"; shift 2 ;;
    --rules-branch) [ "$#" -ge 2 ] || die "--rules-branch needs a value"; BRANCH_FOR_RULES="$2"; shift 2 ;;
    --self-test) MODE=selftest; shift ;;
    -h|--help)   usage; exit 0 ;;
    *)           die "unknown argument '$1' (try --help)" ;;
  esac
done

case "$MODE" in
  selftest) self_test; exit $? ;;
  json)
    [ -f "$JSON_FILE" ] || die "no such file: $JSON_FILE"
    [ -n "$REQUIRED" ] || die "--from-json requires --required"
    classify "$(cat "$JSON_FILE")" "$REQUIRED"
    ;;
  sha)
    [ -n "$REPO" ] || die "--repo (or \$GITHUB_REPOSITORY) is required with --sha"
    if [ -z "$REQUIRED" ]; then
      REQUIRED="$(required_from_ruleset "$REPO" "$BRANCH_FOR_RULES")"
    fi
    [ "$(jq 'length' <<<"$REQUIRED")" -gt 0 ] \
      || die "no required status checks declared for $REPO@$BRANCH_FOR_RULES — refusing to report an empty gate"
    classify "$(gh api "repos/$REPO/commits/$SHA/check-runs?per_page=100")" "$REQUIRED"
    ;;
  *) usage >&2; exit 2 ;;
esac
