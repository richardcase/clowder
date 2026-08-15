#!/usr/bin/env bash
# Classify a change range as `product` (needs the full macOS build) or `site-only` (does not).
#
# The macOS job is runs-on: macos-15 — a 10x Actions-minute multiplier on a private repo — so a
# typo in site/ must not trigger it. That saving is only safe if the classification is trustworthy,
# hence --self-test: this is a pure function of a path list, and CI is a terrible place to discover
# it is wrong.
#
# The rule is an ALLOWLIST: only `site/**` is cheap, and everything else — including docs/ — is
# product. A docs-only change therefore still pays for a macOS build. That is deliberate: widening
# the allowlist widens the set of changes that can reach `main` without the real build having run,
# and docs-only pull requests are rare enough that the trade is not close.
#
# Usage: scripts/changed-scope.sh <base-ref> <head-ref> [branch]
#        scripts/changed-scope.sh --self-test
set -euo pipefail

# cs_drain: discard the rest of stdin. Called before every early return in cs_classify, below,
# for one reason: this function is always the read end of a pipe from `git diff`, and once the
# verdict is known there is no correctness reason to keep reading. But `return`ing while `git
# diff` is still mid-write closes our read end early; the next write it attempts gets SIGPIPE,
# and under `set -o pipefail` that non-zero *writer* exit becomes the whole pipeline's exit
# status even though we already echoed the correct verdict. Draining keeps the read end open
# until `git diff` reaches EOF on its own, so it always exits 0. `|| true` because a downstream
# reader (here, /dev/null) can never itself trigger a SIGPIPE we'd need to swallow, but keeping
# this a no-fail statement means a future change to what `cs_drain` reads into can't reintroduce
# the same problem via `set -e`. See self-test's "…, huge diff, no SIGPIPE" cases — the existing
# 1-2 line fixtures are far too small to exceed a pipe buffer and could not have caught this.
cs_drain() { cat >/dev/null || true; }

# cs_classify <branch> < <newline-separated paths> -> `product` | `site-only`
cs_classify() {
  local branch="${1:-}" path any=0

  # A release branch always gets the real build. It happens to touch VERSION and Cargo.lock today,
  # so it would classify as product anyway — but that is a property of the current file set, not a
  # guarantee, and the failure mode is a release merging on a check that built nothing.
  case "$branch" in
    release/*) cs_drain; echo product; return 0 ;;
  esac

  # The `|| [ -n "$path" ]` matters: `read` returns non-zero on a final line with no trailing
  # newline, which would silently drop it and misclassify a one-file change as `product`.
  while IFS= read -r path || [ -n "$path" ]; do
    [ -n "$path" ] || continue
    any=1
    case "$path" in
      site/*) ;;
      *) cs_drain; echo product; return 0 ;;
    esac
  done

  # No files changed at all (an empty range, or a push to main comparing against itself). Fail safe
  # to the real build rather than waving through a range we could not read.
  if [ "$any" -eq 1 ]; then echo site-only; else echo product; fi
}

self_test() {
  local pass=0 fail=0
  check() {
    local want="$1" name="$2" branch="$3" paths="$4" got
    got="$(printf '%s\n' "$paths" | cs_classify "$branch")"
    if [ "$got" = "$want" ]; then
      echo "  ok    $name ($got)"
      pass=$((pass + 1))
    else
      echo "  FAIL  $name — wanted $want, got $got" >&2
      fail=$((fail + 1))
    fi
  }

  echo "changed-scope: verifying the classification"

  check product   'rust source'            feature 'crates/clowder-daemon/src/main.rs'
  check product   'swift source'           feature 'macos/Sources/ClowderCore/BackendPlan.swift'
  check product   'the workflows'          feature '.github/workflows/ci.yml'
  check product   'docs are not cheap'     feature 'docs/versioning.md'
  check product   'a root file'            feature 'VERSION'
  check site-only 'site markdown'          feature 'site/README.md'
  check site-only 'several site files'     feature 'site/src/pages/index.astro
site/package.json'
  check product   'site plus product'      feature 'site/README.md
crates/clowder-proto/src/lib.rs'

  # A path that merely starts with the string `site` is NOT under site/.
  check product   'sitemap at the root'    feature 'sitemap.xml'
  check product   'a sibling dir'          feature 'site-notes/x.md'

  # Fail-safe cases.
  check product   'empty range'            feature ''
  check product   'blank lines only'       feature '

'
  # Release branches never take the cheap path, whatever they touch.
  check product   'release branch'         release/v0.7.0 'site/README.md'

  # Regression coverage for cs_drain (see its comment above `cs_classify`): an early return
  # that leaves `git diff` still writing gets that writer SIGPIPE'd, and `set -o pipefail` turns
  # that non-zero *writer* exit into this whole pipeline's exit status — even though the correct
  # verdict already reached stdout. `check`, above, only asserts stdout, so it cannot catch this;
  # the fixtures there are also only 1-2 lines, nowhere near a pipe buffer. These fixtures are
  # ~100KB (comfortably over the few-KB buffer that triggers it), and this helper asserts the
  # exit status too.
  check_no_sigpipe() {
    local want="$1" name="$2" branch="$3" paths="$4" got status
    if got="$(printf '%s\n' "$paths" | cs_classify "$branch")"; then
      status=0
    else
      status=$?
    fi
    if [ "$status" -ne 0 ]; then
      echo "  FAIL  $name — exited $status (want 0; SIGPIPE would be 141); stdout was '$got'" >&2
      fail=$((fail + 1))
    elif [ "$got" != "$want" ]; then
      echo "  FAIL  $name — wanted $want, got $got" >&2
      fail=$((fail + 1))
    else
      echo "  ok    $name ($got, exit $status)"
      pass=$((pass + 1))
    fi
  }

  # Mid-stream case: the first path is already non-site, so cs_classify knows the verdict after
  # line 1 — everything after that is exactly what a buggy early `return` would leave stranded,
  # unread, in the pipe.
  local huge_mixed
  huge_mixed="crates/clowder-daemon/src/main.rs"$'\n'"$(printf 'site/file-%d.txt\n' $(seq 1 5000))"
  check_no_sigpipe product 'product, huge diff, no SIGPIPE' feature "$huge_mixed"

  # release/* case: this one returns before reading anything from stdin at all — a second,
  # easier-to-miss instance of the same bug, distinct from the mid-stream case above.
  local huge_release
  huge_release="$(printf 'site/file-%d.txt\n' $(seq 1 5000))"
  check_no_sigpipe product 'release branch, huge diff, no SIGPIPE' release/v0.7.0 "$huge_release"

  echo "changed-scope: $pass passed, $fail failed"
  [ "$fail" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit $? ;;
  -h | --help)
    echo "Usage: scripts/changed-scope.sh <base-ref> <head-ref> [branch]"
    echo "       scripts/changed-scope.sh --self-test"
    exit 0
    ;;
esac

[ "$#" -ge 2 ] || { echo "error: need <base-ref> <head-ref>" >&2; exit 2; }

BASE="$1"
HEAD_REF="$2"
BRANCH="${3:-}"

if ! merge_base="$(git merge-base "$BASE" "$HEAD_REF" 2>/dev/null)"; then
  # No common ancestor means we cannot tell what changed. Build everything.
  echo product
  exit 0
fi

git diff --name-only "$merge_base" "$HEAD_REF" | cs_classify "$BRANCH"
