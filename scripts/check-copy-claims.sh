#!/usr/bin/env bash
# Two jobs, both about the FAQ's honest limitation claims (site/src/components/Faq.astro):
#
#   1. Gap closure. An entry that states a limitation can carry `gap: <issue>` — a promise that the
#      limitation holds only while that issue is open. When the issue closes, the claim is stale by
#      construction: someone fixed the thing and did not update the marketing copy. This has already
#      happened twice (M0, M1). A closed gap fails CI so whoever closed the issue rewrites the answer
#      in the same change, instead of the correction waiting for someone to notice the site is wrong.
#
#   2. Fragment contradiction. A release-note fragment added in this PR (site/src/content/unreleased/)
#      that shares distinctive wording with an OPEN gap's issue title is a prompt, not a hard stop on
#      its own merits — it usually means the gap is about to close and the FAQ entry needs updating in
#      the same PR. This repo's issues are written in almost the FAQ's own words (see #87, the
#      historical miss this check exists for), so title-vs-fragment overlap is a cheap, effective
#      signal.
#
# --self-test exists for the same reason it exists in check-release-notes.sh: a check that never
# fires looks exactly like a check that passed. Isolating everything except the actual GitHub API
# call behind issue_state() is what makes that self-test run with no network at all.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

FAQ_FILE='site/src/components/Faq.astro'
FRAGMENT_DIR='site/src/content/unreleased'
LABEL='no-copy-review'

die() { echo "error: $*" >&2; exit 2; }

# Stopwords for significant_words, below. Measured against the five notes in
# site/src/content/releases/ at the time this check was written:
#   agent   5/5 notes   terminal   4/5   daemon   2/5   pane   2/5   window   2/5   host   1/5
# All are domain-ubiquitous — words a note about almost ANY change is likely to contain — so keeping
# them would make the check fire on unrelated PRs. `host`/`hosts`/`zero` were added after the first
# tuning pass: issue #55's title ("M9c — PTY-host true zero-disruption agent survival") yields `host`
# and `zero` once split, and `host` alone matches v0.6.0's "Manage remote hosts" — a shipped feature,
# not the gap #55 tracks. `m8`/`m9c` (milestone tokens) are listed for documentation even though the
# 4-character minimum in significant_words already drops them — a future edit to that minimum should
# not silently un-drop them without someone noticing this list exists.
STOPWORDS=' agent agents terminal terminals daemon daemons pane panes window windows clowder support true when the with that this from into your host hosts zero m8 m9c '

# ------------------------------------------------------------------------- pure functions

# parse_gaps <faq-file> -> prints "issue<TAB>question" per line, one per `gap:` annotation.
#
# Tracks the most recently seen `q: '...'` line so a `gap:` line under it can be reported with the
# question it belongs to — a failure that only names an issue number sends whoever reads it hunting
# through the FAQ for which entry that was. A malformed value (`gap: abc`, or `gap:` with nothing
# after it) is rejected LOUDLY via die() rather than skipped: a `gap:` nobody is actually watching
# because it failed to parse is worse than no `gap:` at all — it reads as "this is being tracked"
# while tracking nothing.
#
# Two independent guards keep this from firing on content that has nothing to do with gap tracking
# — both matter, and neither substitutes for the other:
#
#   - The array boundary (in_array) is STRUCTURAL, not a literal-string match on `] as const;`. A
#     literal match breaks the moment the file is reformatted so `]` and `as const;` land on
#     separate lines: in_array would never return to 0, the scan would walk into the file's <style>
#     block, and its CSS `gap` property (flex/grid spacing — this file has one) would die as a
#     malformed annotation. Detecting the close as "first non-whitespace character is `]`" survives
#     `as const;` moving, a trailing comment, or the semicolon changing.
#   - The annotation line itself is ANCHORED to the whole line (`^...gap:...$`), not matched as a
#     substring anywhere in it. An unanchored match treats prose like `a: 'There is a gap: between
#     panes…'` as an annotation attempt, fails the numeric check, and dies on a copy edit that has
#     nothing to do with gap tracking. Anchoring also means a commented-out `// gap: 56,` no longer
#     matches at all (the line starts with `//`, not `gap:`) — it's just not an annotation, not a
#     malformed one.
parse_gaps() {
  local file="$1" question='' line value raw in_array=0
  [ -f "$file" ] || die "no such file: $file"
  while IFS= read -r line; do
    if [ "$in_array" -eq 0 ]; then
      case "$line" in
        *'const faqs = ['*) in_array=1 ;;
      esac
      continue
    fi
    # Structural close: first non-whitespace character is `]`, independent of what follows it on
    # the line (`as const;`, a trailing comment, nothing at all).
    if [[ "$line" =~ ^[[:space:]]*\] ]]; then
      in_array=0
      continue
    fi
    if [[ "$line" =~ q:[[:space:]]*\'([^\']*)\' ]]; then
      question="${BASH_REMATCH[1]}"
    elif [[ "$line" =~ ^[[:space:]]*gap:[[:space:]]*([^,/]*),?[[:space:]]*(//.*)?$ ]]; then
      # Capture the raw match into a plain variable BEFORE any further [[ =~ ]] test — a second
      # regex test (the numeric check below) overwrites BASH_REMATCH even when it fails, so reading
      # BASH_REMATCH[1] again afterward for the error message would hit an unset element under
      # `set -u` instead of reporting what was actually found.
      raw="${BASH_REMATCH[1]}"
      value="$(printf '%s' "$raw" | tr -d '[:space:]')"
      [[ "$value" =~ ^[0-9]+$ ]] \
        || die "malformed gap: annotation for '$question' in $file: '$raw' is not an issue number"
      printf '%s\t%s\n' "$value" "$question"
    fi
  done < "$file"
}

# significant_words <title> -> prints the words worth matching on, one per line: lowercased, split on
# any run of non-alphanumeric characters, stopwords and anything under 4 characters dropped.
significant_words() {
  local title="$1" cleaned word
  cleaned="$(printf '%s' "$title" | tr '[:upper:]' '[:lower:]' | tr -c '[:alnum:]' ' ')"
  for word in $cleaned; do
    [ "${#word}" -ge 4 ] || continue
    case "$STOPWORDS" in
      *" $word "*) continue ;;
    esac
    printf '%s\n' "$word"
  done
}

# contradicts <fragment-text> <title> -> 0 if a significant word of the title appears as a PREFIX of
# some word in the fragment, 1 otherwise. One match is enough (see the module comment above).
#
# The comparison truncates each significant word to its first 5 characters before checking the
# prefix (only the title's word — fragment words are compared in full). Plain whole-word prefix
# matching gets `reflow` -> `reflows` right (reflow IS a literal prefix of reflows) but misses
# `resized` -> `resizing`: English drops the silent `e` before `-ing`, so "resize" is NOT a literal
# prefix of "resizing" (they diverge at the 6th character: resiz-E vs resiz-I). Both pairs share a
# 5-character stem ("reflo", "resiz"), and 5 is short enough to absorb -s/-ed/-ing without a real
# stemmer, but long enough that short, specific gap words (e.g. "linux", exactly 5 characters) are
# compared in full rather than truncated into something generic. Verified against both required
# cases and empirically tuned against the real corpus — see task-1-report.md Step 4.
contradicts() {
  local fragment="$1" title="$2" fragment_lc fwords word stem fword
  fragment_lc="$(printf '%s' "$fragment" | tr '[:upper:]' '[:lower:]')"
  fwords="$(printf '%s' "$fragment_lc" | tr -c '[:alnum:]' ' ')"
  while IFS= read -r word; do
    [ -n "$word" ] || continue
    stem="${word:0:5}"
    for fword in $fwords; do
      case "$fword" in
        "$stem"*) return 0 ;;
      esac
    done
  done < <(significant_words "$title")
  return 1
}

# issue_state <number> -> prints "state<TAB>title" on stdout (state lowercased "open"/"closed"), or
# fails with a non-zero exit and the underlying tool's own error on stderr. The ONE function in this
# script that touches the network — everything above is pure so --self-test needs no connectivity.
# CHECK_COPY_CLAIMS_ISSUE_STATE_CMD lets a caller (the self-test, or an operator debugging offline)
# point this at a stub instead of `gh`; state and title travel together so a single lookup serves
# both the gap-closure check and the contradiction check's title-matching, rather than fetching the
# same issue twice. Namespaced with the script's own name, not a generic ISSUE_STATE_CMD: a bare
# name risks a stray environment variable silently swapping the real `gh` call for a stub in a REAL
# run, not just under --self-test. Deliberately still honoured outside --self-test too (not gated
# to it) — the offline-debugging use case is real and the long, script-specific name already makes
# an accidental collision implausible; gating it would remove that use case for a marginal further
# reduction in an already-small risk.
issue_state() {
  local n="$1" raw
  if [ -n "${CHECK_COPY_CLAIMS_ISSUE_STATE_CMD:-}" ]; then
    "$CHECK_COPY_CLAIMS_ISSUE_STATE_CMD" "$n"
    return $?
  fi
  raw="$(gh issue view "$n" --json state,title --jq '(.state | ascii_downcase) + "\t" + .title')" || return 1
  printf '%s\n' "$raw"
}

# ------------------------------------------------------------------ thin orchestration (impure)

# check_gap_closure <faq-file>
#   stdout: "issue<TAB>state<TAB>title" per gap: annotation (state "open"/"closed") — reused by the
#           contradiction check below so each gap issue is fetched exactly once.
#   stderr: one line per problem (a closed gap, or a lookup failure), or a note when there are no
#           gap: annotations at all.
#   returns 1 if any gap is closed or its lookup failed, 0 otherwise.
check_gap_closure() {
  local faq="$1" gaps status=0 issue question result state title
  # Explicit `if !` check, not a bare `gaps="$(parse_gaps "$faq")"` relied on for `set -e` to catch:
  # this function is itself called as `gap_lines="$(check_gap_closure ...)" || status=1` from main,
  # and bash disables -e for the ENTIRE body of a function invoked in a context whose own exit
  # status is being tested (here, by that `||`) — including nested command substitutions inside it.
  # A bare assignment would let parse_gaps's die() (loud rejection of a malformed `gap:` value)
  # print its message and then have this function silently carry on as if there were no gaps at
  # all, which is precisely the "silently ignored annotation" parse_gaps exists to prevent.
  # Reproduced and confirmed against this exact double-nesting shape before this fix landed.
  if ! gaps="$(parse_gaps "$faq")"; then
    echo "copy-claims: FAIL — could not read gap: annotations from $faq (see the parse error above)" >&2
    return 1
  fi
  if [ -z "$gaps" ]; then
    echo "copy-claims: no gap: annotations in $faq — nothing to watch" >&2
    return 0
  fi
  while IFS=$'\t' read -r issue question; do
    [ -n "$issue" ] || continue
    if ! result="$(issue_state "$issue")"; then
      echo "copy-claims: FAIL — could not look up #$issue (FAQ entry '$question'); a GitHub API error or an expired/missing token is the likely cause" >&2
      status=1
      continue
    fi
    state="${result%%$'\t'*}"
    title="${result#*$'\t'}"
    printf '%s\t%s\t%s\n' "$issue" "$state" "$title"
    if [ "$state" = "closed" ]; then
      echo "copy-claims: FAIL — FAQ entry '$question' is annotated gap: $issue, but #$issue is now closed; rewrite the answer in the same change that closed it" >&2
      status=1
    fi
  done <<< "$gaps"
  return "$status"
}

# check_fragment_contradictions <gap-lines> <fragment-path>...
#   gap-lines: "issue<TAB>state<TAB>title" per line, as produced by check_gap_closure. Only OPEN
#   gaps are matched against — a closed gap has already failed check_gap_closure, so flagging it
#   again here would just be noise stacked on the real failure.
#   returns 1 if any fragment contradicts any open gap's title.
check_fragment_contradictions() {
  local gap_lines="$1" status=0
  shift
  [ -n "$gap_lines" ] || return 0
  local frag text gissue gstate gtitle
  for frag in "$@"; do
    [ -n "$frag" ] || continue
    [ -f "$frag" ] || continue
    text="$(cat "$frag")"
    while IFS=$'\t' read -r gissue gstate gtitle; do
      [ -n "$gissue" ] || continue
      [ "$gstate" = "open" ] || continue
      if contradicts "$text" "$gtitle"; then
        echo "copy-claims: FAIL — $frag shares wording with open gap #$gissue ('$gtitle'); if this change closes that gap, update the FAQ entry in the same pull request" >&2
        status=1
      fi
    done <<< "$gap_lines"
  done
  return "$status"
}

# added_fragments <base> <head> -> prints paths of release-note fragments ADDED in the range. Mirrors
# check-release-notes.sh's added_fragments: restricted to *.md so a `.gitkeep` placeholder added to
# keep the (possibly empty) directory in git is never scanned for prose it doesn't contain.
added_fragments() {
  git diff --name-only --diff-filter=A "$1" "$2" -- "$FRAGMENT_DIR/*.md" || true
}

# ------------------------------------------------------------------------- self-test
self_test() {
  local pass=0 fail=0 tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  check() {
    local name="$1" want="$2" got="$3"
    if [ "$got" = "$want" ]; then
      echo "  ok    $name"
      pass=$((pass + 1))
    else
      echo "  FAIL  $name — wanted $want, got $got" >&2
      fail=$((fail + 1))
    fi
  }

  # A stub for issue_state — never touches the network. #55/#56/#87 use their real titles so the
  # contradiction cases exercise the actual gap titles this check will run against in CI.
  stub_issue_state() {
    case "$1" in
      55) printf 'open\tM9c — PTY-host true zero-disruption agent survival\n' ;;
      56) printf 'open\tM8 — Linux support\n' ;;
      87) printf 'closed\tTerminal grid does not reflow when the window is resized\n' ;;
      99) printf 'closed\tSome long-fixed limitation\n' ;;
      *) return 1 ;;
    esac
  }
  CHECK_COPY_CLAIMS_ISSUE_STATE_CMD=stub_issue_state

  echo "check-copy-claims: verifying the pure core"

  # ---- contradicts: prefix matching (required cases) ----
  local got
  # Deliberately avoids other significant words of the title ("grid", "resized") so this case
  # isolates the reflow/reflows prefix match rather than incidentally passing via an exact match on
  # one of them.
  if contradicts 'The terminal now reflows correctly.' 'Terminal grid does not reflow when the window is resized'; then got=fail; else got=pass; fi
  check "contradicts: reflow title matches reflows fragment" fail "$got"

  if contradicts 'Resizing a pane updates the layout immediately.' 'Terminal grid does not reflow when the window is resized'; then got=fail; else got=pass; fi
  check "contradicts: resized title matches Resizing fragment" fail "$got"

  # ---- contradicts: stopword suppression (Ruling 1 / C1) ----
  if contradicts 'This release fixes a small agent bug.' 'M9c — PTY-host true zero-disruption agent survival'; then got=fail; else got=pass; fi
  check "contradicts: bare 'agent' vs #55 title is suppressed" pass "$got"

  # ---- contradicts: no shared significant word ----
  if contradicts 'Install with Homebrew: brew install --cask clowder.' 'M8 — Linux support'; then got=fail; else got=pass; fi
  check "contradicts: unrelated fragment does not match" pass "$got"

  # ---- parse_gaps: malformed values rejected loudly, not skipped ----
  local faq="$tmp/faq-malformed.astro" out rc
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    gap: abc,
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then rc=0; else rc=$?; fi
  if [ "$rc" -ne 0 ] && [[ "$out" == *"malformed"* ]]; then got=pass; else got=fail; fi
  check "parse_gaps: 'gap: abc' dies loudly" pass "$got"

  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    gap: ,
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then rc=0; else rc=$?; fi
  if [ "$rc" -ne 0 ] && [[ "$out" == *"malformed"* ]]; then got=pass; else got=fail; fi
  check "parse_gaps: empty 'gap:' dies loudly" pass "$got"

  # ---- parse_gaps: scoped to the faqs array, not the whole .astro file ----
  # Faq.astro's own <style> block sets the CSS `gap` property (flex/grid spacing). An unscoped scan
  # would treat "gap: 1px;" as a malformed annotation and die on the site's real FAQ file — this
  # reproduces that shape directly rather than trusting the real file to keep failing the same way.
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
  },
] as const;
---
<style>
  .faq__list {
    display: flex;
    gap: 1px;
  }
</style>
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then got=pass; else got=fail; fi
  check "parse_gaps: CSS 'gap:' outside the array is not an annotation" pass "$got"

  # ---- parse_gaps: array close survives reformatting (Finding 1) ----
  # `]` and `as const;` on SEPARATE lines — a literal `*'] as const;'*` match never fires here, so
  # in_array would stay 1 forever and the scan would walk into the CSS gap below and die. The
  # structural check ("first non-whitespace char is ]") must close the array on the bare `]` line
  # regardless of what (if anything) follows it.
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
  },
]
as const;
---
<style>
  .faq__list {
    gap: 1px;
  }
</style>
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [ "$out" = "$(printf '55\tWhat happens to my agents when I close the window?')" ]; then got=pass; else got=fail; fi
  check "parse_gaps: array close survives ] and as const; on separate lines" pass "$got"

  # ---- parse_gaps: prose containing 'gap:' does not break the check (Finding 2) ----
  # An FAQ answer using the word "gap" in an ordinary sentence must not be mistaken for an
  # annotation attempt — only a line that IS a gap: annotation (anchored, start to end) counts.
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Will splits overlap?',
    a: 'There is a gap: between panes so they never touch.',
  },
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [ "$out" = "$(printf '55\tWhat happens to my agents when I close the window?')" ]; then got=pass; else got=fail; fi
  check "parse_gaps: prose containing 'gap:' is not treated as an annotation" pass "$got"

  # ---- parse_gaps: a commented-out annotation is not live (Minor 3, resolved by the Finding 2 anchor) ----
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    // gap: 56,
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [ -z "$out" ]; then got=pass; else got=fail; fi
  check "parse_gaps: a commented-out '// gap: 56,' is not a live annotation" pass "$got"

  # ---- check_gap_closure: a malformed gap: must still fail loudly through the SAME calling shape
  # main() uses (`gap_lines="$(check_gap_closure ...)" || status=1`). A bare, unguarded assignment
  # inside check_gap_closure passed the earlier two malformed-value cases (they call parse_gaps
  # directly) while still silently swallowing the failure end-to-end, because bash disables -e for
  # an entire function body when the function's own call is itself inside a tested context like
  # this `||` — confirmed by reproducing exactly this double-nesting before the fix landed.
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    gap: abc,
  },
] as const;
EOF
  local end_to_end_status=0
  out="$(check_gap_closure "$faq" 2>&1)" || end_to_end_status=1
  if [ "$end_to_end_status" -eq 1 ] && [[ "$out" == *"malformed"* ]]; then got=pass; else got=fail; fi
  check "check_gap_closure: malformed gap: fails through main's ||-wrapped call shape" pass "$got"

  # ---- check_gap_closure: closed / open / mixed / empty / API error ----
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    gap: 99,
  },
] as const;
EOF
  if out="$(check_gap_closure "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = fail ] && [[ "$out" == *"'Does it work on Linux or Windows?'"* ]] && [[ "$out" == *"#99"* ]]; then got=pass; else got=fail; fi
  check "check_gap_closure: closed gap fails, naming entry and issue" pass "$got"

  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
  },
] as const;
EOF
  if check_gap_closure "$faq" >/dev/null 2>&1; then got=pass; else got=fail; fi
  check "check_gap_closure: open gap passes" pass "$got"

  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Which agents can it run?',
    a: 'Claude Code, OpenAI Codex, and a plain shell.',
  },
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
  },
] as const;
EOF
  if check_gap_closure "$faq" >/dev/null 2>&1; then got=pass; else got=fail; fi
  check "check_gap_closure: entry with no gap: is ignored, rest still passes" pass "$got"

  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Is Clowder open source?',
    a: 'No.',
  },
] as const;
EOF
  if out="$(check_gap_closure "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [[ "$out" == *"no gap:"* ]]; then got=pass; else got=fail; fi
  check "check_gap_closure: no gap: annotations at all passes and says so" pass "$got"

  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'A question about issue 404.',
    a: 'An answer.',
    gap: 404,
  },
] as const;
EOF
  out="$(check_gap_closure "$faq" 2>&1)" && got=pass || got=fail
  if [ "$got" = fail ] && [[ "$out" == *"API error"* ]]; then got=pass; else got=fail; fi
  check "check_gap_closure: issue_state failure fails, naming API error as a cause" pass "$got"

  unset -f stub_issue_state
  unset CHECK_COPY_CLAIMS_ISSUE_STATE_CMD

  echo "check-copy-claims: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

# ------------------------------------------------------------------------- CLI

case "${1:-}" in
  --self-test) self_test; exit $? ;;
  -h | --help)
    cat <<'EOF'
Usage: scripts/check-copy-claims.sh [<base-ref> [<head-ref>]]   (default: origin/main HEAD)
       scripts/check-copy-claims.sh --self-test
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

status=0
gap_lines="$(check_gap_closure "$FAQ_FILE")" || status=1

# Guard possibly-empty array expansion under set -u (bash 3.2 idiom, scripts/sign-app.sh:30-33):
# `mapfile`/readarray isn't available in 3.2, so build the array with a while-read loop instead.
fragments=()
while IFS= read -r f; do
  [ -n "$f" ] || continue
  fragments+=("$f")
done < <(added_fragments "$merge_base" "$HEAD_REF")

if [ "${#fragments[@]}" -gt 0 ]; then
  check_fragment_contradictions "$gap_lines" "${fragments[@]}" || status=1
fi

if [ "$status" -ne 0 ]; then
  case ",${PR_LABELS:-}," in
    *",$LABEL,"*)
      echo "copy-claims: ok  '$LABEL' label present — skipping by explicit choice"
      exit 0
      ;;
  esac
  echo "copy-claims: FAIL — see above. Rewrite the stale claim, or add the '$LABEL' label if this was reviewed and needs no change." >&2
  exit 1
fi

echo "copy-claims: ok"
exit 0
