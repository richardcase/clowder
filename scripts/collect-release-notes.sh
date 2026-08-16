#!/usr/bin/env bash
# Collect accumulated release-note fragments into one per-version file at release time.
#
# Fragments accumulate one-per-PR in site/src/content/unreleased/*.md (each added and guarded by
# scripts/check-release-notes.sh, back at the PR that made the user-visible change). This script is
# what a release actually runs: concatenate every fragment in sorted filename order, stamp it with
# the version and date the site's `releases` collection schema expects, delete the fragments that
# went into it, and leave `.gitkeep` behind so the now-empty directory still exists for the next PR
# — git does not track empty directories, and a release that silently un-tracks unreleased/ would
# break the very check that requires a fragment on the next feat/fix PR.
#
# The no-fragments case is not an error: a patch release that is pure `fix` commits with internal
# `no-release-note`-labelled PRs (a CI tweak, a refactor) is completely legitimate, and a script that
# refused to release without user-visible prose would either block real releases or train people to
# write filler notes — worse than no note, per check-release-notes.sh's own message. It writes
# "Maintenance and fixes." and exits 0.
#
# The content guard (check-release-notes.sh --file) runs on the COLLECTED file, not each fragment
# individually, on purpose: a fragment could in principle be clean on its own and only leak once
# concatenated next to another (not true of today's patterns, but the guard's job is the boundary
# the collected file crosses, and that is the file that ships to a public page — checking it, not
# its inputs, is what actually matters). It runs against a temp file BEFORE anything real is
# touched: the destination is only written and fragments are only deleted once the guard passes, so
# a violation leaves the unreleased/ directory and the releases/ directory exactly as they were.
#
# Usage: scripts/collect-release-notes.sh [--force] <version>
#        scripts/collect-release-notes.sh --self-test
#
# RELEASE_DATE overrides today's date (else `date -u +%Y-%m-%d`), so the self-test is deterministic
# and a caller can pin the date a release actually shipped rather than the date the script happened
# to run.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHECK_SCRIPT="$ROOT/scripts/check-release-notes.sh"

# Same grammar as scripts/set-version.sh's own check, copied rather than sourced: that script has no
# library form to source, and this is a one-line regex, not logic worth factoring out for two
# call sites. If it ever drifts, --self-test's malformed-version cases catch it.
VERSION_PATTERN='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'

die() { echo "error: $*" >&2; exit 2; }

usage() {
  cat <<'EOF'
Usage: scripts/collect-release-notes.sh [--force] <version>
       scripts/collect-release-notes.sh --self-test
EOF
}

version_ok() {
  [[ $1 =~ $VERSION_PATTERN ]]
}

# fragments_in <dir> -> prints *.md paths NUL-delimited, in sorted filename order
#
# NUL-delimited (not newline) because a fragment's slug is a free-form filename and nothing stops
# one containing a space; `find -print0` / `sort -z` is the only combination that survives that.
# `LC_ALL=C` pins byte-order collation so the release this produces does not depend on which
# machine — a contributor's laptop locale vs. CI's — happened to run the release.
# A missing directory prints nothing rather than erroring: a release after the previous one already
# swept every fragment (or before this feature existed at all) is not a failure, it is the common
# case. Restricted to *.md so `.gitkeep` — and anything else that is not a fragment — is never
# collected or deleted; scripts/check-release-notes.sh's added_fragments applies the same
# restriction on the write side, for the matching reason (see its comment).
fragments_in() {
  local dir="$1"
  [ -d "$dir" ] || return 0
  find "$dir" -maxdepth 1 -type f -name '*.md' -print0 | LC_ALL=C sort -z
}

# collect <version> <fragment-dir> <out-file> -> writes out-file, deletes consumed fragments
#
# Parameterized on both directories (rather than hardcoding the real site paths) so --self-test can
# run this against a scratch tree instead of the real, non-reproducible content the brief says not
# to touch during development.
collect() {
  local version="$1" fragment_dir="$2" out_file="$3"
  local release_date="${RELEASE_DATE:-$(date -u +%Y-%m-%d)}"
  local tmp frag files=() first=1

  while IFS= read -r -d '' frag; do
    files+=("$frag")
  done < <(fragments_in "$fragment_dir")

  tmp="$(mktemp)"
  {
    printf -- '---\n'
    printf "version: '%s'\n" "$version"
    printf "date: '%s'\n" "$release_date"
    printf -- '---\n\n'
    if [ "${#files[@]}" -eq 0 ]; then
      # A patch release of pure fix commits is legitimate and must not be blocked for lack of user-
      # visible prose to report — see the file header.
      printf 'Maintenance and fixes.\n'
    else
      for frag in "${files[@]}"; do
        if [ "$first" -eq 1 ]; then first=0; else printf '\n'; fi
        cat "$frag"
      done
    fi
  } > "$tmp"

  # Guard the collected file BEFORE touching anything real. A violation must leave both the
  # fragments and the releases directory exactly as they were, so a failed release can be retried
  # after a fix rather than needing fragments manually restored from git.
  #
  # `return 1` here, not `die` (which calls `exit`) — this function is invoked from inside `if`
  # conditions (both call sites, and --self-test's guard case), and `exit` inside a function called
  # from an `if` still terminates the whole process rather than just failing the condition, which
  # would abort --self-test after the first failure instead of tallying it.
  if ! "$CHECK_SCRIPT" --file "$tmp" >&2; then
    rm -f "$tmp"
    echo "error: collected notes for $version failed the content guard (see above) — nothing was written or deleted" >&2
    return 1
  fi

  mkdir -p "$(dirname "$out_file")"
  mv "$tmp" "$out_file"

  # Guarded on count, not bare `"${files[@]}"`: under `set -u`, bash 3.2 (the maintainer's
  # `/bin/bash`) treats an empty array's `[@]` expansion as an unbound variable and aborts — exactly
  # the no-fragments path this script exists to make legitimate, not exceptional. Same idiom as
  # scripts/sign-app.sh's `${kc[@]+"${kc[@]}"}` (see its comment); a plain length check reads more
  # naturally here since the loop body wants ordinary `"${files[@]}"`, not a single expansion site.
  if [ "${#files[@]}" -gt 0 ]; then
    for frag in "${files[@]}"; do
      rm -f "$frag"
    done
  fi
}

# ----------------------------------------------------------------------------- self-test
self_test() {
  local pass=0 fail=0 tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  # A pass here is only evidence for the bash that ran it — bash's own empty-array-under-`set -u`
  # behaviour changed between 3.2 (macOS's `/bin/bash`, still the maintainer's default) and later
  # versions, so a bug in that exact class can pass under one and abort under the other with no
  # summary line at all (found in review: the fragment-deletion loop crashed on bash 3.2 while this
  # suite was 17/17 on Homebrew bash 5). Printing the version makes that visible in the log instead
  # of silent; it does not by itself prove portability; run this explicitly with `/bin/bash
  # scripts/collect-release-notes.sh --self-test` — not just `bash …` — since `bash` may not
  # resolve to 3.2 even on macOS. See the fix-round note in task-4-report.md for what else was
  # considered here (a self re-exec under /bin/bash) and why it was rejected.
  echo "collect-release-notes: self-test running under bash $BASH_VERSION ($0)"

  ok()   { echo "  ok    $1"; pass=$((pass + 1)); }
  bad()  { echo "  FAIL  $1 — $2" >&2; fail=$((fail + 1)); }

  # fresh_case <name> -> sets $CASE_DIR/{unreleased,releases} and cds nowhere (paths are absolute)
  fresh_case() {
    CASE_DIR="$tmp/$1"
    mkdir -p "$CASE_DIR/unreleased" "$CASE_DIR/releases"
  }

  echo "collect-release-notes: verifying fragment collection"

  # --- sorted concatenation ---
  fresh_case sorted
  printf -- '- Zebra last alphabetically, written first on disk.\n' > "$CASE_DIR/unreleased/zzz.md"
  printf -- '- Alpha comes first by filename.\n' > "$CASE_DIR/unreleased/aaa.md"
  if collect 1.2.3 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.2.3.md" 2>"$tmp/sorted.err"; then
    body="$(sed '1,5d' "$CASE_DIR/releases/v1.2.3.md")"
    want="- Alpha comes first by filename.

- Zebra last alphabetically, written first on disk."
    if [ "$body" = "$want" ]; then
      ok "sorted concatenation (alpha before zebra, filename order not disk order)"
    else
      bad "sorted concatenation" "got: $body"
    fi
  else
    bad "sorted concatenation" "collect failed: $(cat "$tmp/sorted.err")"
  fi

  # --- deletes consumed fragments but never .gitkeep ---
  fresh_case gitkeep
  printf -- '- One consumed fragment.\n' > "$CASE_DIR/unreleased/note.md"
  printf -- '# keep me\n' > "$CASE_DIR/unreleased/.gitkeep"
  collect 1.0.0 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.0.0.md" 2>"$tmp/gitkeep.err" || true
  if [ ! -f "$CASE_DIR/unreleased/note.md" ] && [ -f "$CASE_DIR/unreleased/.gitkeep" ]; then
    ok "consumed fragment deleted, .gitkeep survives"
  else
    bad "consumed fragment deleted, .gitkeep survives" \
      "note.md exists=$( [ -f "$CASE_DIR/unreleased/note.md" ] && echo yes || echo no ), .gitkeep exists=$( [ -f "$CASE_DIR/unreleased/.gitkeep" ] && echo yes || echo no )"
  fi

  # --- no fragments -> "Maintenance and fixes." and exit 0 ---
  fresh_case empty
  printf -- '# keep me\n' > "$CASE_DIR/unreleased/.gitkeep"
  if collect 1.0.1 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.0.1.md" 2>"$tmp/empty.err"; then
    body="$(sed '1,5d' "$CASE_DIR/releases/v1.0.1.md")"
    if [ "$body" = "Maintenance and fixes." ]; then
      ok "no fragments -> 'Maintenance and fixes.', exit 0"
    else
      bad "no fragments -> 'Maintenance and fixes.'" "got: $body"
    fi
  else
    bad "no fragments -> exit 0" "collect exited nonzero: $(cat "$tmp/empty.err")"
  fi

  # --- missing unreleased/ directory behaves like an empty one ---
  fresh_case missing
  rm -rf "$CASE_DIR/unreleased"
  if collect 1.0.2 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.0.2.md" 2>"$tmp/missing.err"; then
    body="$(sed '1,5d' "$CASE_DIR/releases/v1.0.2.md")"
    if [ "$body" = "Maintenance and fixes." ]; then
      ok "missing unreleased/ directory behaves like empty"
    else
      bad "missing unreleased/ directory behaves like empty" "got: $body"
    fi
  else
    bad "missing unreleased/ directory behaves like empty" "collect exited nonzero: $(cat "$tmp/missing.err")"
  fi

  # --- frontmatter shape, so a schema regression is caught here and not by `npm run check` ---
  fresh_case frontmatter
  if collect 2.5.9 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v2.5.9.md" 2>"$tmp/fm.err"; then
    head="$(head -n4 "$CASE_DIR/releases/v2.5.9.md")"
    want="---
version: '2.5.9'
date: '${RELEASE_DATE:-$(date -u +%Y-%m-%d)}'
---"
    if [ "$head" = "$want" ]; then
      ok "frontmatter matches the { version, date } schema"
    else
      bad "frontmatter matches the { version, date } schema" "got: $head"
    fi
  else
    bad "frontmatter matches the { version, date } schema" "collect failed: $(cat "$tmp/fm.err")"
  fi

  # --- runs the content guard on the result, and rejects a leak rather than writing/deleting ---
  fresh_case guarded
  printf -- '- See https://github.com/richardcase/clowder/pull/72 for detail.\n' > "$CASE_DIR/unreleased/leak.md"
  if collect 1.0.3 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.0.3.md" >"$tmp/guarded.out" 2>&1; then
    bad "content guard rejects a leaking fragment" "collect succeeded; should have failed the guard"
  else
    if [ -f "$CASE_DIR/releases/v1.0.3.md" ]; then
      bad "content guard leaves nothing written on failure" "v1.0.3.md exists despite the guard failing"
    elif [ ! -f "$CASE_DIR/unreleased/leak.md" ]; then
      bad "content guard leaves the fragment undeleted on failure" "leak.md was deleted despite the guard failing"
    else
      ok "content guard rejects a leaking fragment, writes/deletes nothing"
    fi
  fi

  # --- malformed version is rejected by the CLI (main, not collect — collect is unaware of it) ---
  echo
  echo "collect-release-notes: verifying CLI-level version validation"
  check_version() {
    local want="$1" name="$2" version="$3" got
    if version_ok "$version"; then got=ok; else got=reject; fi
    if [ "$got" = "$want" ]; then
      ok "$name ($got)"
    else
      bad "$name" "wanted $want, got $got"
    fi
  }
  check_version ok     'plain semver'          '1.2.3'
  check_version ok     'zero version'          '0.6.1'
  check_version ok     'prerelease suffix'     '1.2.3-rc1'
  check_version ok     'dotted prerelease'     '1.2.3-alpha.1'
  check_version reject 'missing patch'         '1.2'
  check_version reject 'leading v'             'v1.2.3'
  check_version reject 'leading zero'          '01.2.3'
  check_version reject 'trailing junk'         '1.2.3junk'
  check_version reject 'empty string'          ''

  # --- refuses to overwrite an existing version file without --force ---
  fresh_case overwrite
  printf -- '- Original note.\n' > "$CASE_DIR/unreleased/note.md"
  collect 1.5.0 "$CASE_DIR/unreleased" "$CASE_DIR/releases/v1.5.0.md" >/dev/null 2>&1
  printf -- '- New note that must not silently replace the old one.\n' > "$CASE_DIR/unreleased/note2.md"
  # Exercise the CLI's own overwrite guard (main, not collect — see run_cli below), pointed at the
  # scratch tree via env overrides so the real repo content is never touched.
  if COLLECT_FRAGMENT_DIR="$CASE_DIR/unreleased" COLLECT_RELEASES_DIR="$CASE_DIR/releases" \
      "$0" 1.5.0 >"$tmp/overwrite.out" 2>&1; then
    bad "refuses to overwrite without --force" "CLI exited 0; should have refused"
  else
    if grep -q 'Original note' "$CASE_DIR/releases/v1.5.0.md"; then
      ok "refuses to overwrite without --force (original file untouched)"
    else
      bad "refuses to overwrite without --force" "v1.5.0.md was modified despite refusing"
    fi
  fi
  if COLLECT_FRAGMENT_DIR="$CASE_DIR/unreleased" COLLECT_RELEASES_DIR="$CASE_DIR/releases" \
      "$0" --force 1.5.0 >"$tmp/force.out" 2>&1; then
    if grep -q 'New note that must not silently replace' "$CASE_DIR/releases/v1.5.0.md"; then
      ok "--force overwrites an existing version file"
    else
      bad "--force overwrites an existing version file" "content unchanged: $(cat "$CASE_DIR/releases/v1.5.0.md")"
    fi
  else
    bad "--force overwrites an existing version file" "CLI exited nonzero: $(cat "$tmp/force.out")"
  fi

  echo
  echo "collect-release-notes: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

# ----------------------------------------------------------------------------- CLI
# run_cli <args...> — the real entry point. Split from `main` sourcing purely so --self-test's
# overwrite cases can invoke "$0" as a subprocess (needed to test the CLI's own arg parsing and exit
# status) without that subprocess re-running --self-test itself.
#
# COLLECT_FRAGMENT_DIR / COLLECT_RELEASES_DIR override the real site paths — used only by the
# --self-test subprocess above, so it can drive the CLI end to end against a scratch tree instead of
# site/src/content/{unreleased,releases}, which the brief says development must not touch.
run_cli() {
  local force=0 version=''
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --force) force=1; shift ;;
      -h | --help) usage; exit 0 ;;
      --) shift; break ;;
      -*) die "unknown option '$1' (try --help)" ;;
      *) [ -z "$version" ] || die "unexpected extra argument '$1'"; version="$1"; shift ;;
    esac
  done
  [ -n "$version" ] || { usage >&2; exit 2; }
  version_ok "$version" || die "version '$version' is not X.Y.Z[-prerelease]"

  local fragment_dir="${COLLECT_FRAGMENT_DIR:-$ROOT/site/src/content/unreleased}"
  local releases_dir="${COLLECT_RELEASES_DIR:-$ROOT/site/src/content/releases}"
  local out_file="$releases_dir/v$version.md"

  if [ -e "$out_file" ] && [ "$force" -ne 1 ]; then
    die "$out_file already exists — pass --force to overwrite (a re-dispatched release must not silently double-collect)"
  fi

  collect "$version" "$fragment_dir" "$out_file"
  echo "collect-release-notes: wrote $out_file"
}

case "${1:-}" in
  --self-test) self_test; exit $? ;;
esac

run_cli "$@"
