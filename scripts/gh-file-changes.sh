#!/usr/bin/env bash
# Describe the working tree's changes against a ref as the file-list shape GitHub's
# `createCommitOnBranch` GraphQL mutation wants: {"additions":[{"path":…,"contents":<base64>}],
# "deletions":[{"path":…}]}.
#
# Exists because the `bump` job's "Commit the bump" step used to hardcode a 3-file addition list
# with no deletions field at all — correct only as long as set-version.sh was the only thing that
# ever touched the working tree. Once scripts/collect-release-notes.sh runs in the same job it also
# DELETES consumed fragments, which that literal had no way to express, and a hand-maintained list
# silently drops anything new that set-version.sh starts touching. This script instead reads
# whatever the working tree actually says changed, so the mutation's file list is always in sync
# with reality.
#
# Usage: scripts/gh-file-changes.sh [<ref>]      (default ref: HEAD)
#        scripts/gh-file-changes.sh --self-test
set -euo pipefail

die() { echo "error: $*" >&2; exit 2; }

usage() {
  cat <<'EOF'
Usage: scripts/gh-file-changes.sh [<ref>]
       scripts/gh-file-changes.sh --self-test
EOF
}

# b64 <file> -> base64 of the file's current bytes, single line, no trailing newline.
#
# `base64 -w0` (no wrapping) is GNU-only. `bump` runs on ubuntu-latest so it IS available there, but
# this script's own self-test must also pass on macOS, where BSD base64 has no `-w` at all. Rather
# than branch on platform, pipe through `tr -d '\n'`: it strips whatever line-wrapping either
# implementation chose (BSD base64 does not wrap by default; GNU without -w0 wraps at 76 cols), so
# the result is identical either way and is exactly what the GraphQL mutation needs — one base64
# string, no embedded newlines.
b64() {
  base64 <"$1" | tr -d '\n'
}

# file_changes <root> [<ref>] -> JSON {"additions":[...],"deletions":[...]} on stdout
#
# Parameterized on root (rather than hardcoding the caller's cwd) so --self-test can run this
# against a scratch git repo instead of the real working tree — same reason
# scripts/collect-release-notes.sh parameterizes on directories instead of hardcoding
# site/src/content.
#
# Enumeration is `git status --porcelain=v1 -z`: NUL-delimited because a changed path is a free-form
# filename and nothing stops one containing a space (the whole reason -z exists over the default
# newline-delimited, C-quoted format), and porcelain=v1 covers added/modified/deleted files whether
# staged or not, plus untracked files, in one pass.
file_changes() {
  local root_arg="$1" ref="${2:-HEAD}"
  local root head resolved
  # Resolve to the repo TOPLEVEL rather than trusting the argument: `git status --porcelain` paths
  # are always toplevel-relative, never cwd-relative. Joining them onto anything else — e.g. a
  # subdirectory passed in by a caller — silently looks up the wrong absolute path, so an existing,
  # modified file reads as absent and gets reported as a DELETION instead of an addition. Today's
  # only caller passes the repo root already, so this cannot fire in practice, but nothing enforced
  # that, and getting it wrong here means a modification landing in the signed bump commit as a
  # deletion. See the "called from a subdirectory" self-test case below.
  root="$(git -C "$root_arg" rev-parse --show-toplevel)"
  head="$(git -C "$root" rev-parse HEAD)"
  resolved="$(git -C "$root" rev-parse "$ref")"
  # `git status` has no notion of "against an arbitrary ref" — it only ever reports the checked-out
  # HEAD against the index and worktree. So <ref> is a sanity check here, not a diff target: the
  # real caller (the bump job) checks out exactly base_sha as HEAD and never commits before calling
  # this script, so ref == HEAD always holds there. Refusing a mismatch is more honest than silently
  # reporting HEAD's diff while claiming it belongs to a different ref.
  # `return 1` here, not `die` (which calls `exit`) — this function is invoked from inside an `if`
  # condition in --self-test's ref-mismatch case, and `exit` inside a function called from an `if`
  # still terminates the whole process rather than just failing the condition (same footgun
  # scripts/collect-release-notes.sh's `collect` avoids for the same reason).
  if [ "$resolved" != "$head" ]; then
    echo "error: ref '$ref' resolves to $resolved, but HEAD is $head — git status can only report against the checked-out HEAD; check out $ref first" >&2
    return 1
  fi

  local entries=()
  # --no-renames: without it, a rename shows as a single R-typed record (two NUL-separated paths,
  # old then new) that this parser does not understand. With it, the same change decomposes for
  # free into a plain delete-of-old + add-of-new pair, which is exactly the "reject a rename by
  # treating it as delete + add" requirement — no special-case code needed.
  # --untracked-files=all: the default ("normal") collapses a wholly-new directory into one entry
  # for the directory itself rather than each file in it, which would make `[ -e "$abspath" ]` below
  # try to base64 a directory. "all" always lists individual files.
  while IFS= read -r -d '' entry; do
    [ -n "$entry" ] || continue
    entries+=("$entry")
  done < <(git -C "$root" status --porcelain=v1 -z --no-renames --untracked-files=all)

  local additions_file deletions_file
  additions_file="$(mktemp)"
  deletions_file="$(mktemp)"
  # Ensure both exist and are empty even when there are zero entries — jq --slurpfile on a missing
  # file errors, and an empty (zero-JSON-value) file is exactly what should slurp to `[]`.
  : >"$additions_file"
  : >"$deletions_file"

  # Guarded on count, not a bare `"${entries[@]}"`: under `set -u`, bash 3.2 (the maintainer's
  # `/bin/bash`) treats an empty array's `[@]` expansion as an unbound variable and aborts — exactly
  # the "nothing changed" case this script must report as `{"additions":[],"deletions":[]}`, not a
  # crash. Same idiom as scripts/collect-release-notes.sh's fragment-deletion loop.
  if [ "${#entries[@]}" -gt 0 ]; then
    local entry path abspath
    for entry in "${entries[@]}"; do
      # Porcelain v1's record shape is fixed: 2 status chars, 1 space, then the path — slicing at a
      # literal offset rather than parsing the status chars, because the addition-vs-deletion call
      # below is made on disk state, not on which of the ~15 XY combinations (staged/unstaged/added/
      # modified/deleted, and their pairings) produced this record.
      path="${entry:3}"
      abspath="$root/$path"
      # What ends up in the commit is the CURRENT working-tree state, so "does this path exist on
      # disk right now" is the right test — not the status letters. It is also what makes a staged
      # delete (`D `), an unstaged delete (` D`), and a rename's decomposed old-path delete (see
      # --no-renames above) all fall out as deletions through the same branch, with no per-code
      # special-casing.
      if [ -e "$abspath" ]; then
        jq -cn --arg path "$path" --arg contents "$(b64 "$abspath")" \
          '{path: $path, contents: $contents}' >>"$additions_file"
      else
        jq -cn --arg path "$path" '{path: $path}' >>"$deletions_file"
      fi
    done
  fi

  # --slurpfile reads every JSON value in a file into an array; a file with zero values (the no-op
  # case above) slurps to `[]`, which is why an empty change set does not need its own branch here.
  jq -cn --slurpfile additions "$additions_file" --slurpfile deletions "$deletions_file" \
    '{additions: $additions, deletions: $deletions}'
  rm -f "$additions_file" "$deletions_file"
}

# ----------------------------------------------------------------------------- self-test
self_test() {
  local pass=0 fail=0 tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  # Same reasoning as scripts/collect-release-notes.sh's self-test preamble: bash's empty-array
  # behaviour under `set -u` differs between 3.2 (macOS's `/bin/bash`, still the maintainer's
  # default) and later versions, so print which one ran rather than leaving that silent.
  echo "gh-file-changes: self-test running under bash $BASH_VERSION ($0)"

  ok()  { echo "  ok    $1"; pass=$((pass + 1)); }
  bad() { echo "  FAIL  $1 — $2" >&2; fail=$((fail + 1)); }

  # repo <name> -> creates $REPO as a fresh git repo with an initial empty commit, cds nowhere
  # (all git calls below go through `git -C "$REPO"` so cwd never has to move).
  repo() {
    REPO="$tmp/$1"
    mkdir -p "$REPO"
    git -C "$REPO" init -q
    git -C "$REPO" config user.email test@test.com
    git -C "$REPO" config user.name Test
    # Global commit.gpgsign is true on the maintainer's machine; a scratch repo has no signing key
    # configured, so without this every commit below would fail.
    git -C "$REPO" config commit.gpgsign false
  }
  commit_all() { git -C "$REPO" add -A && git -C "$REPO" commit -q -m "$1"; }

  echo "gh-file-changes: verifying change enumeration"

  # --- empty change set ---
  repo empty
  printf 'one\n' >"$REPO/a.txt"
  commit_all seed
  got="$(file_changes "$REPO")"
  want='{"additions":[],"deletions":[]}'
  if [ "$got" = "$want" ]; then
    ok "empty change set -> {\"additions\":[],\"deletions\":[]}"
  else
    bad "empty change set" "got: $got"
  fi

  # --- untracked file is an addition, base64 round-trips ---
  repo untracked
  printf 'seed\n' >"$REPO/a.txt"
  commit_all seed
  printf 'hello world\n' >"$REPO/new-file.txt"
  got="$(file_changes "$REPO")"
  path="$(jq -r '.additions[0].path' <<<"$got")"
  contents="$(jq -r '.additions[0].contents' <<<"$got")"
  decoded="$(printf '%s' "$contents" | base64 -d)"
  if [ "$(jq '.additions | length' <<<"$got")" = 1 ] && [ "$(jq '.deletions | length' <<<"$got")" = 0 ] \
      && [ "$path" = "new-file.txt" ] && [ "$decoded" = $'hello world' ]; then
    ok "untracked file -> single addition, base64 round-trips"
  else
    bad "untracked file -> addition" "got: $got (decoded: $decoded)"
  fi

  # --- modified tracked file (unstaged) carries the CURRENT content ---
  repo modified-unstaged
  printf 'v1\n' >"$REPO/a.txt"
  commit_all seed
  printf 'v2\n' >"$REPO/a.txt"
  got="$(file_changes "$REPO")"
  decoded="$(jq -r '.additions[0].contents' <<<"$got" | base64 -d)"
  if [ "$(jq '.additions | length' <<<"$got")" = 1 ] && [ "$decoded" = $'v2' ]; then
    ok "modified tracked file (unstaged) -> addition with current content"
  else
    bad "modified tracked file (unstaged)" "got: $got"
  fi

  # --- modified tracked file (staged) also counts ---
  repo modified-staged
  printf 'v1\n' >"$REPO/a.txt"
  commit_all seed
  printf 'v2\n' >"$REPO/a.txt"
  git -C "$REPO" add a.txt
  got="$(file_changes "$REPO")"
  if [ "$(jq '.additions | length' <<<"$got")" = 1 ] && [ "$(jq -r '.additions[0].path' <<<"$got")" = "a.txt" ]; then
    ok "modified tracked file (staged) -> addition"
  else
    bad "modified tracked file (staged)" "got: $got"
  fi

  # --- staged delete and unstaged delete both count as deletions, no contents field ---
  repo deleted-staged
  printf 'v1\n' >"$REPO/a.txt"
  printf 'v1\n' >"$REPO/b.txt"
  commit_all seed
  git -C "$REPO" rm -q a.txt
  rm -f "$REPO/b.txt"
  got="$(file_changes "$REPO")"
  ok_shape=1
  jq -e '(.deletions | length) == 2' <<<"$got" >/dev/null || ok_shape=0
  jq -e '(.additions | length) == 0' <<<"$got" >/dev/null || ok_shape=0
  jq -e 'all(.deletions[]; has("path") and (has("contents") | not))' <<<"$got" >/dev/null || ok_shape=0
  if [ "$ok_shape" = 1 ]; then
    ok "staged delete + unstaged delete -> deletions with path only"
  else
    bad "staged delete + unstaged delete" "got: $got"
  fi

  # --- rename decomposes into delete-of-old + add-of-new, no R status leaks through ---
  repo renamed
  printf 'content\n' >"$REPO/old-name.txt"
  commit_all seed
  git -C "$REPO" mv old-name.txt new-name.txt
  got="$(file_changes "$REPO")"
  ok_shape=1
  [ "$(jq '.additions | length' <<<"$got")" = 1 ] || ok_shape=0
  [ "$(jq '.deletions | length' <<<"$got")" = 1 ] || ok_shape=0
  [ "$(jq -r '.additions[0].path' <<<"$got")" = "new-name.txt" ] || ok_shape=0
  [ "$(jq -r '.deletions[0].path' <<<"$got")" = "old-name.txt" ] || ok_shape=0
  if [ "$ok_shape" = 1 ]; then
    ok "rename -> delete(old) + add(new), no rename status leaks through"
  else
    bad "rename -> delete + add" "got: $got"
  fi

  # --- untracked new file inside a wholly-new directory is listed individually ---
  repo new-dir
  printf 'seed\n' >"$REPO/a.txt"
  commit_all seed
  mkdir -p "$REPO/sub/dir"
  printf 'nested\n' >"$REPO/sub/dir/nested.txt"
  got="$(file_changes "$REPO")"
  if [ "$(jq '.additions | length' <<<"$got")" = 1 ] && [ "$(jq -r '.additions[0].path' <<<"$got")" = "sub/dir/nested.txt" ]; then
    ok "untracked file in a new directory is listed individually, not as the directory"
  else
    bad "untracked file in a new directory" "got: $got"
  fi

  # --- a path containing a space survives the NUL-delimited pipeline ---
  repo spacey
  printf 'seed\n' >"$REPO/a.txt"
  commit_all seed
  printf 'hi\n' >"$REPO/has space.txt"
  got="$(file_changes "$REPO")"
  if [ "$(jq -r '.additions[0].path' <<<"$got")" = "has space.txt" ]; then
    ok "a path containing a space is preserved"
  else
    bad "a path containing a space" "got: $got"
  fi

  # --- mixed add + modify + delete in one change set, all in one JSON object ---
  repo mixed
  printf 'v1\n' >"$REPO/keep.txt"
  printf 'v1\n' >"$REPO/gone.txt"
  commit_all seed
  printf 'v2\n' >"$REPO/keep.txt"
  rm -f "$REPO/gone.txt"
  printf 'brand new\n' >"$REPO/fresh.txt"
  got="$(file_changes "$REPO")"
  ok_shape=1
  [ "$(jq '.additions | length' <<<"$got")" = 2 ] || ok_shape=0
  [ "$(jq '.deletions | length' <<<"$got")" = 1 ] || ok_shape=0
  jq -e '(.additions | map(.path) | sort) == ["fresh.txt","keep.txt"]' <<<"$got" >/dev/null || ok_shape=0
  [ "$(jq -r '.deletions[0].path' <<<"$got")" = "gone.txt" ] || ok_shape=0
  if [ "$ok_shape" = 1 ]; then
    ok "mixed add + modify + delete in a single change set"
  else
    bad "mixed add + modify + delete" "got: $got"
  fi

  # --- emits valid JSON via jq, matching the mutation's expected shape (the brief's own probe) ---
  repo shape
  printf 'seed\n' >"$REPO/a.txt"
  commit_all seed
  printf 'x\n' >"$REPO/added.txt"
  git -C "$REPO" rm -q a.txt 2>/dev/null || rm -f "$REPO/a.txt"
  got="$(file_changes "$REPO")"
  if jq -e '
      (.additions | type == "array") and
      (.deletions | type == "array") and
      (all(.additions[]; has("path") and has("contents") and (.contents | test("^[A-Za-z0-9+/=]*$")))) and
      (all(.deletions[]; has("path") and (has("contents") | not)))
    ' <<<"$got" >/dev/null; then
    ok "JSON shape matches createCommitOnBranch's fileChanges input"
  else
    bad "JSON shape matches createCommitOnBranch's fileChanges input" "got: $got"
  fi

  # --- called from a subdirectory of the repo still resolves against the toplevel, not cwd ---
  # Regression coverage for the root-resolution bug found in review: passing a subdirectory used to
  # join toplevel-relative `git status` paths onto the wrong base, so a real modification silently
  # became a reported deletion.
  repo subdir-root
  printf 'v1\n' >"$REPO/a.txt"
  mkdir -p "$REPO/sub"
  commit_all seed
  printf 'v2\n' >"$REPO/a.txt"
  got="$(file_changes "$REPO/sub")"
  if [ "$(jq '.additions | length' <<<"$got")" = 1 ] && [ "$(jq '.deletions | length' <<<"$got")" = 0 ] \
      && [ "$(jq -r '.additions[0].path' <<<"$got")" = "a.txt" ]; then
    ok "called from a subdirectory still resolves against the repo toplevel (stays an addition, not a deletion)"
  else
    bad "called from a subdirectory" "got: $got"
  fi

  # --- base64 output is genuinely single-line even for content long enough to trigger wrapping ---
  # Regression coverage from mutation testing: every other fixture here is under 57 bytes, so a
  # mutant that deletes `tr -d '\n'` from b64() survived — nothing exercised the GNU-vs-BSD wrapping
  # difference that function exists to paper over. GNU `base64` without `-w0` wraps at 76 output
  # columns, which corresponds to 57 input bytes; this fixture is well past that.
  repo big-fixture
  printf 'seed\n' >"$REPO/a.txt"
  commit_all seed
  big_content="$(printf 'x%.0s' $(seq 1 500))"
  printf '%s\n' "$big_content" >"$REPO/big.txt"
  got="$(file_changes "$REPO")"
  contents="$(jq -r '.additions[0].contents' <<<"$got")"
  decoded="$(printf '%s' "$contents" | base64 -d)"
  if [ "$(printf '%s' "$contents" | wc -l | tr -d ' ')" = 0 ] && [ "$decoded" = "$big_content" ]; then
    ok "base64 of a >57-byte file is a single line and round-trips"
  else
    bad "base64 of a >57-byte file is a single line and round-trips" \
      "contents had $(printf '%s' "$contents" | wc -l | tr -d ' ') embedded newline(s); decode matched: $([ "$decoded" = "$big_content" ] && echo yes || echo no)"
  fi

  # --- ref other than the checked-out HEAD is refused, not silently misreported ---
  repo ref-mismatch
  printf 'v1\n' >"$REPO/a.txt"
  commit_all first
  first_sha="$(git -C "$REPO" rev-parse HEAD)"
  printf 'v2\n' >"$REPO/a.txt"
  commit_all second
  if file_changes "$REPO" "$first_sha" >"$tmp/mismatch.out" 2>"$tmp/mismatch.err"; then
    bad "ref other than checked-out HEAD is refused" "exited 0: $(cat "$tmp/mismatch.out")"
  else
    if grep -q "can only report against the checked-out HEAD" "$tmp/mismatch.err"; then
      ok "ref other than checked-out HEAD is refused with a clear error"
    else
      bad "ref other than checked-out HEAD is refused" "wrong error: $(cat "$tmp/mismatch.err")"
    fi
  fi

  # --- explicit ref matching HEAD behaves the same as the default ---
  repo ref-match
  printf 'v1\n' >"$REPO/a.txt"
  commit_all seed
  printf 'v2\n' >"$REPO/a.txt"
  head_sha="$(git -C "$REPO" rev-parse HEAD)"
  got_default="$(file_changes "$REPO")"
  got_explicit="$(file_changes "$REPO" "$head_sha")"
  got_head="$(file_changes "$REPO" HEAD)"
  if [ "$got_default" = "$got_explicit" ] && [ "$got_default" = "$got_head" ]; then
    ok "explicit ref matching HEAD (by SHA or by name) matches the default"
  else
    bad "explicit ref matching HEAD" "default: $got_default explicit: $got_explicit head: $got_head"
  fi

  echo
  echo "gh-file-changes: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit $? ;;
  -h | --help) usage; exit 0 ;;
esac

[ "$#" -le 1 ] || die "unexpected extra argument '$2' (try --help)"

file_changes "$(pwd)" "${1:-HEAD}"
