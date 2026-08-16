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

# parse_gaps <faq-file> -> prints "issue<TAB>question<TAB>gapWords" per line, one per `gap:`
# annotation. gapWords is empty when the entry carries no `gapWords:` line.
#
# Tracks the most recently seen `q: '...'` line so a `gap:` line under it can be reported with the
# question it belongs to — a failure that only names an issue number sends whoever reads it hunting
# through the FAQ for which entry that was. A malformed value (`gap: abc`, or `gap:` with nothing
# after it) is rejected LOUDLY via die() rather than skipped: a `gap:` nobody is actually watching
# because it failed to parse is worse than no `gap:` at all — it reads as "this is being tracked"
# while tracking nothing.
#
# `gapWords: '...'` is an OPTIONAL second line under a `gap:`, carrying terms a release note would
# plausibly use about that gap. It exists because issue titles are engineering jargon and release
# notes are deliberately plain language (see AGENTS.md's release-note convention) — they systematically
# don't share words, so title-only matching in check_fragment_contradictions can miss the exact case
# it was built for (a partial fix landing while the tracking issue, correctly, stays open). A
# `gapWords:` with no `gap:` immediately above it is rejected LOUDLY for the same reason a malformed
# `gap:` is: it would read as "this is being tracked" while attaching to nothing.
#
# A `gap:` entry is buffered rather than printed the instant it's matched, specifically so a
# `gapWords:` line immediately under it can still attach — printed output can't be edited after the
# fact. The buffered entry is flushed (printed) on whichever comes first: the next `q:`, the next
# `gap:`, the array's structural close, or a `gapWords:` line completing it (a `gap:` has at most one
# `gapWords:`, so there's nothing further to wait for once it arrives).
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
  local pending_issue='' pending_question='' pending_words='' have_pending=0
  [ -f "$file" ] || die "no such file: $file"

  # Flush the buffered gap: (+ optional gapWords:) entry, if any. A no-op when nothing is pending, so
  # it's safe to call unconditionally at every point an entry might have ended.
  flush() {
    [ "$have_pending" -eq 1 ] || return 0
    printf '%s\t%s\t%s\n' "$pending_issue" "$pending_question" "$pending_words"
    have_pending=0
    pending_words=''
  }

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
      flush
      in_array=0
      continue
    fi
    if [[ "$line" =~ q:[[:space:]]*\'([^\']*)\' ]]; then
      flush
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
      flush
      pending_issue="$value"
      pending_question="$question"
      have_pending=1
    elif [[ "$line" =~ ^[[:space:]]*gapWords:[[:space:]]*\'([^\']*)\',?[[:space:]]*(//.*)?$ ]]; then
      [ "$have_pending" -eq 1 ] \
        || die "gapWords: annotation for '$question' in $file has no preceding gap: to attach to"
      pending_words="${BASH_REMATCH[1]}"
      flush
    elif [[ "$line" =~ ^[[:space:]]*gapWords: ]]; then
      die "malformed gapWords: annotation for '$question' in $file: expected a single-quoted string"
    fi
  done < "$file"
  flush
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

# _issue_lookup_once <number> -> one attempt, stdout "state<TAB>title" on success, stderr + non-zero
# on failure. Factored out of issue_state so the retry wrapper below has exactly one thing to retry.
# CHECK_COPY_CLAIMS_ISSUE_STATE_CMD lets a caller (the self-test, or an operator debugging offline)
# point this at a stub instead of `gh`; state and title travel together so a single lookup serves
# both the gap-closure check and the contradiction check's title-matching, rather than fetching the
# same issue twice. Namespaced with the script's own name, not a generic ISSUE_STATE_CMD: a bare
# name risks a stray environment variable silently swapping the real `gh` call for a stub in a REAL
# run, not just under --self-test. Deliberately still honoured outside --self-test too (not gated
# to it) — the offline-debugging use case is real and the long, script-specific name already makes
# an accidental collision implausible; gating it would remove that use case for a marginal further
# reduction in an already-small risk.
_issue_lookup_once() {
  local n="$1"
  if [ -n "${CHECK_COPY_CLAIMS_ISSUE_STATE_CMD:-}" ]; then
    "$CHECK_COPY_CLAIMS_ISSUE_STATE_CMD" "$n"
    return $?
  fi
  gh issue view "$n" --json state,title --jq '(.state | ascii_downcase) + "\t" + .title'
}

# is_definitive_miss <error-text> -> 0 if the error text says the issue genuinely does not exist, 1
# for anything else. `gh issue view` talks to the GraphQL API, not REST, so there is no bare "404" —
# a nonexistent issue instead comes back as this exact GraphQL error (verified against a live `gh
# issue view 999999`: `GraphQL: Could not resolve to an issue or pull request with the number of
# 999999. (repository.issue)`). That is this script's equivalent of site/src/data/release.ts's
# definitive 4xx: retrying it just spends the delay arriving at the same answer. Everything else —
# HTTP 401/403/5xx, a secondary rate limit, a DNS blip, a bare network error — is transient and
# worth retrying, the same split fetchOnce() makes against a real HTTP status.
is_definitive_miss() {
  case "$1" in
    *'Could not resolve to an issue or pull request'*) return 0 ;;
    *) return 1 ;;
  esac
}

# issue_state <number> -> prints "state<TAB>title" on stdout (state lowercased "open"/"closed"), or
# fails with a non-zero exit and the underlying error on stderr. The ONE function in this script that
# touches the network — everything above is pure so --self-test needs no connectivity.
#
# Retries a transient failure twice (three attempts total) before giving up, with a short delay
# between attempts — the spec calls for this (Milestone 3: "retries twice and then fails") so a
# single 403, secondary rate limit, or DNS blip does not hard-fail `commit-lint` on every open PR at
# once. A definitive miss (see is_definitive_miss) is NOT retried: retrying it cannot change the
# answer, and doing so anyway would just make a genuinely-absent issue read as a real API problem in
# the log. CHECK_COPY_CLAIMS_RETRY_DELAYS overrides the delay ladder (space-separated seconds) so
# --self-test can exercise the retry paths without real sleeps.
issue_state() {
  local n="$1" raw err err_file attempt=1
  local delays=(${CHECK_COPY_CLAIMS_RETRY_DELAYS:-2 5})
  local max_attempts=$(( ${#delays[@]} + 1 ))

  # No `trap ... RETURN` here on purpose: a RETURN trap set inside a function replaces the CALLER's
  # own RETURN trap process-wide for the rest of the run (verified — a trap set in an inner function
  # fires in place of the outer function's own trap when the outer function later returns, with the
  # inner function's now out-of-scope locals, tripping `set -u`). self_test relies on its own
  # `trap ... RETURN` to clean up its tmp dir, so issue_state cleans up explicitly at every return
  # instead of installing a trap that would clobber it.
  err_file="$(mktemp)"

  while :; do
    if raw="$(_issue_lookup_once "$n" 2>"$err_file")"; then
      rm -f "$err_file"
      printf '%s\n' "$raw"
      return 0
    fi
    err="$(cat "$err_file")"
    if is_definitive_miss "$err" || [ "$attempt" -ge "$max_attempts" ]; then
      rm -f "$err_file"
      printf '%s\n' "$err" >&2
      return 1
    fi
    echo "copy-claims: lookup of #$n failed (attempt $attempt/$max_attempts), retrying in ${delays[$((attempt - 1))]}s: $err" >&2
    sleep "${delays[$((attempt - 1))]}"
    attempt=$((attempt + 1))
  done
}

# ------------------------------------------------------------------ thin orchestration (impure)

# check_gap_closure <faq-file>
#   stdout: "issue<TAB>state<TAB>title<TAB>gapWords" per gap: annotation (state "open"/"closed") —
#           reused by the contradiction check below so each gap issue is fetched exactly once.
#   stderr: one line per problem — a closed gap, a lookup failure, or zero gap: annotations at all.
#   returns 1 if any gap is closed, its lookup failed, or there are zero gap: annotations; 0 otherwise.
check_gap_closure() {
  local faq="$1" gaps status=0 issue question words result state title
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
    # FAIL, not pass: this script's own header warns that a `gap:` nobody is watching is worse than
    # no `gap:` at all, and a silent pass here is exactly that failure mode turned up to eleven — a
    # reformat, a rewrite, or an unaware edit that drops every gap: annotation leaves this job GREEN
    # while it guards nothing, with no signal anywhere that the guard is gone. Two are expected today
    # (#55, #56); if a future PR genuinely removes the last one on purpose (both tracked limitations
    # actually fixed), that PR can say so with the no-copy-review label like any other change here —
    # the same escape hatch, not a silent zero-annotation pass carved out just for this case.
    echo "copy-claims: FAIL — no gap: annotations in $faq; either every tracked limitation was fixed and this is expected (add the no-copy-review label), or a reformat/edit silently dropped them — this check exists precisely so a claim nobody is watching doesn't go unnoticed, and an empty result is indistinguishable from that until a human says otherwise" >&2
    return 1
  fi
  while IFS=$'\t' read -r issue question words; do
    [ -n "$issue" ] || continue
    if ! result="$(issue_state "$issue")"; then
      echo "copy-claims: FAIL — could not look up #$issue (FAQ entry '$question'); a GitHub API error or an expired/missing token is the likely cause" >&2
      status=1
      continue
    fi
    state="${result%%$'\t'*}"
    title="${result#*$'\t'}"
    printf '%s\t%s\t%s\t%s\n' "$issue" "$state" "$title" "$words"
    if [ "$state" = "closed" ]; then
      echo "copy-claims: FAIL — FAQ entry '$question' is annotated gap: $issue, but #$issue is now closed; rewrite the answer in the same change that closed it" >&2
      status=1
    fi
  done <<< "$gaps"
  return "$status"
}

# check_fragment_contradictions <gap-lines> <fragment-path>...
#   gap-lines: "issue<TAB>state<TAB>title<TAB>gapWords" per line, as produced by check_gap_closure.
#   Only OPEN gaps are matched against — a closed gap has already failed check_gap_closure, so
#   flagging it again here would just be noise stacked on the real failure.
#   returns 1 if any fragment contradicts any open gap's title (or its gapWords — see contradicts's
#   caller below, which matches against the two concatenated).
check_fragment_contradictions() {
  local gap_lines="$1" status=0
  shift
  [ -n "$gap_lines" ] || return 0
  local frag text gissue gstate gtitle gwords
  for frag in "$@"; do
    [ -n "$frag" ] || continue
    [ -f "$frag" ] || continue
    text="$(cat "$frag")"
    while IFS=$'\t' read -r gissue gstate gtitle gwords; do
      [ -n "$gissue" ] || continue
      [ "$gstate" = "open" ] || continue
      # gwords is appended to the title before matching, not passed separately: significant_words
      # already lowercases, splits on non-alnum, and drops stopwords/short words, so concatenating
      # plain-language gapWords terms onto the title and running them through the same filter needs
      # no new logic — see parse_gaps's gapWords: comment for why title matching alone can miss this.
      if contradicts "$text" "$gtitle $gwords"; then
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
  # Zero delay so the retry-path cases below (and any incidental retry through stub_issue_state's
  # default `return 1` arm) run instantly rather than sleeping for real seconds.
  CHECK_COPY_CLAIMS_RETRY_DELAYS='0 0'

  echo "check-copy-claims: verifying the pure core"

  # ---- issue_state: retries a transient failure, does not retry a definitive one (Finding 1) ----
  #
  # Call counts are tallied through a FILE, not a shell variable: issue_state's own retry loop calls
  # _issue_lookup_once inside a `$(...)` command substitution, which is a subshell, so a variable the
  # stub increments there is discarded the instant that subshell exits — the parent's copy never
  # moves. A file survives the subshell boundary the same way $tmp already does for the fixtures
  # above. (Caught by running this exact shape stand-alone before landing it: with a plain variable,
  # every attempt logged as "call 1" and the retry count read back as 0.)
  local out2 got calls_file="$tmp/retry-calls"

  bump_calls() { printf '%s' "$(($(cat "$calls_file") + 1))" > "$calls_file"; cat "$calls_file"; }

  stub_flaky_then_ok() {
    local n; n="$(bump_calls)"
    if [ "$n" -lt 3 ]; then
      echo "simulated transient failure (DNS blip)" >&2
      return 1
    fi
    printf 'open\tSome Title\n'
  }
  printf '0' > "$calls_file"
  CHECK_COPY_CLAIMS_ISSUE_STATE_CMD=stub_flaky_then_ok
  if out2="$(issue_state 1 2>/dev/null)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [ "$out2" = "$(printf 'open\tSome Title')" ] && [ "$(cat "$calls_file")" -eq 3 ]; then got=pass; else got=fail; fi
  check "issue_state: retries a transient failure twice, succeeds on the 3rd attempt" pass "$got"

  stub_always_fails() {
    bump_calls >/dev/null
    echo "simulated transient failure (503)" >&2
    return 1
  }
  printf '0' > "$calls_file"
  CHECK_COPY_CLAIMS_ISSUE_STATE_CMD=stub_always_fails
  if issue_state 1 >/dev/null 2>&1; then got=fail; else got=pass; fi
  if [ "$got" = pass ] && [ "$(cat "$calls_file")" -eq 3 ]; then got=pass; else got=fail; fi
  check "issue_state: a persistent transient failure fails after 3 attempts (2 retries)" pass "$got"

  stub_definitive_miss() {
    bump_calls >/dev/null
    echo 'GraphQL: Could not resolve to an issue or pull request with the number of 1. (repository.issue)' >&2
    return 1
  }
  printf '0' > "$calls_file"
  CHECK_COPY_CLAIMS_ISSUE_STATE_CMD=stub_definitive_miss
  if issue_state 1 >/dev/null 2>&1; then got=fail; else got=pass; fi
  if [ "$got" = pass ] && [ "$(cat "$calls_file")" -eq 1 ]; then got=pass; else got=fail; fi
  check "issue_state: a definitive miss (issue genuinely absent) is NOT retried" pass "$got"

  unset -f stub_flaky_then_ok stub_always_fails stub_definitive_miss bump_calls
  CHECK_COPY_CLAIMS_ISSUE_STATE_CMD=stub_issue_state

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
  if [ "$got" = pass ] && [ "$out" = "$(printf '55\tWhat happens to my agents when I close the window?\t')" ]; then got=pass; else got=fail; fi
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
  if [ "$got" = pass ] && [ "$out" = "$(printf '55\tWhat happens to my agents when I close the window?\t')" ]; then got=pass; else got=fail; fi
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

  # ---- parse_gaps: gapWords: attaches to the preceding gap: (Finding 2) ----
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
    // Terms a release note would plausibly use about this gap.
    gapWords: 'resume restore vanish',
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then got=pass; else got=fail; fi
  if [ "$got" = pass ] && [ "$out" = "$(printf '55\tWhat happens to my agents when I close the window?\tresume restore vanish')" ]; then got=pass; else got=fail; fi
  check "parse_gaps: gapWords: attaches to the preceding gap: annotation" pass "$got"

  # ---- parse_gaps: gapWords: with no preceding gap: dies loudly, same philosophy as a malformed
  # gap: — an annotation nobody is actually watching is worse than none at all. ----
  cat > "$faq" <<'EOF'
const faqs = [
  {
    q: 'Does it work on Linux or Windows?',
    a: 'Not yet.',
    gapWords: 'resume restore vanish',
  },
] as const;
EOF
  if out="$(parse_gaps "$faq" 2>&1)"; then rc=0; else rc=$?; fi
  if [ "$rc" -ne 0 ] && [[ "$out" == *"no preceding gap:"* ]]; then got=pass; else got=fail; fi
  check "parse_gaps: gapWords: with no preceding gap: dies loudly" pass "$got"

  # ---- check_fragment_contradictions: gapWords catches what title-only matching cannot (Finding 2)
  # ----
  # The concrete miss this exists for: v0.4.0's real release note describes partial daemon-restart
  # recovery in plain language while #55 ("M9c — PTY-host true zero-disruption agent survival") stays
  # open — title-only matching shares ZERO words with that note (disruption/survival/pty all miss).
  # gapWords carries the plain-language terms the note actually uses.
  #
  # gap_lines_gw is produced by the REAL check_gap_closure (stub_issue_state answers #55), not typed
  # in by hand: a hand-rolled 4th field would still limp through the OLD, gapWords-unaware
  # check_fragment_contradictions, because bash's `read` folds any extra fields into the last named
  # variable — a 3-var `read` given 4 tab-separated fields quietly appends the 4th to gtitle instead
  # of erroring. Routing through parse_gaps -> check_gap_closure is what makes the "does this actually
  # need the fix" question honest: unfixed parse_gaps has no gapWords: handling at all, so this path
  # is the one that genuinely can't fake a pass.
  local faq_gw gap_lines_gw frag_hit frag_nohit
  faq_gw="$tmp/faq-gapwords.astro"
  cat > "$faq_gw" <<'EOF'
const faqs = [
  {
    q: 'What happens to my agents when I close the window?',
    a: 'They keep running.',
    gap: 55,
    gapWords: 'resume restore vanish',
  },
] as const;
EOF
  gap_lines_gw="$(check_gap_closure "$faq_gw" 2>/dev/null)"

  frag_hit="$tmp/frag-hit.md"
  cat > "$frag_hit" <<'EOF'
If the daemon has to restart, the agents that were running come back on their own, with their
conversation resumed and their split layout restored, instead of just vanishing.
EOF
  # Confirms the miss is real before confirming the fix: title alone (no gapWords) does not catch
  # this fragment — if it did, the case below would prove nothing about gapWords specifically.
  if contradicts "$(cat "$frag_hit")" 'M9c — PTY-host true zero-disruption agent survival'; then got=fail; else got=pass; fi
  check "check_fragment_contradictions: title alone misses the v0.4.0-shaped note (the bug)" pass "$got"

  if check_fragment_contradictions "$gap_lines_gw" "$frag_hit" >/dev/null 2>&1; then got=fail; else got=pass; fi
  check "check_fragment_contradictions: gapWords catches the note title matching alone misses" pass "$got"

  frag_nohit="$tmp/frag-nohit.md"
  cat > "$frag_nohit" <<'EOF'
Manage remote hosts from a dedicated Settings pane: add and pair them over TLS, then switch between
local and remote from the sidebar connection chip.
EOF
  if check_fragment_contradictions "$gap_lines_gw" "$frag_nohit" >/dev/null 2>&1; then got=pass; else got=fail; fi
  check "check_fragment_contradictions: unrelated fragment matches neither title nor gapWords" pass "$got"

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
  # Zero gap: annotations FAILS (Minor 3) rather than passing silently: a reformat or an unaware edit
  # that drops every annotation must not leave this job green while it guards nothing.
  if out="$(check_gap_closure "$faq" 2>&1)"; then got=fail; else got=pass; fi
  if [ "$got" = pass ] && [[ "$out" == *"no gap:"* ]]; then got=pass; else got=fail; fi
  check "check_gap_closure: zero gap: annotations FAILS rather than passing silently" pass "$got"

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
  unset CHECK_COPY_CLAIMS_RETRY_DELAYS

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
