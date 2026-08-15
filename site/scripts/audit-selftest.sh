#!/usr/bin/env bash
#
# Tests the tests. audit.sh guards two invariants that are invisible locally,
# and its dangerous failure mode is not a false alarm but a SILENT PASS — a
# pattern that matches nothing still exits 0 and prints `ok`.
#
# That is not hypothetical. Check 1 hardcodes the GitHub org in its pattern; it
# reported `ok` while checking nothing at all after the repo moved from
# richardcase/* to defiantsoftware/*, because the old owner no longer appeared
# in the built HTML. A green build proved nothing.
#
# So each check is exercised against a fixture that MUST fail, and one that MUST
# pass. Run via `npm test`.
set -euo pipefail

AUDIT="$(cd "$(dirname "$0")" && pwd)/audit.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

# Build a fixture dir containing $2 as index.html, then assert audit.sh's exit
# status matches $1 ("pass" = exits 0, "fail" = exits non-zero).
check() {
  local want="$1" name="$2" body="$3"
  local dir="$WORK/$name"
  mkdir -p "$dir"
  printf '%s\n' "$body" > "$dir/index.html"

  local got
  if "$AUDIT" "$dir" >/dev/null 2>&1; then got=pass; else got=fail; fi

  if [[ "$got" == "$want" ]]; then
    echo "  ok    $name (audit $got, as expected)"
    pass=$((pass + 1))
  else
    echo "  FAIL  $name — expected audit to $want, but it $got" >&2
    fail=$((fail + 1))
  fi
}

# Build a fixture dir containing a file named $3 (with innocuous content), then assert audit.sh's
# exit status matches $1. check() above always writes index.html, so it cannot express a fixture
# whose *filename* is the violation.
check_file() {
  local want="$1" name="$2" filename="$3"
  local dir="$WORK/$name"
  mkdir -p "$dir"
  printf '<html></html>\n' > "$dir/index.html"
  printf 'nothing to see here\n' > "$dir/$filename"

  local got
  if "$AUDIT" "$dir" >/dev/null 2>&1; then got=pass; else got=fail; fi

  if [[ "$got" == "$want" ]]; then
    echo "  ok    $name (audit $got, as expected)"
    pass=$((pass + 1))
  else
    echo "  FAIL  $name — expected audit to $want, but it $got" >&2
    fail=$((fail + 1))
  fi
}

echo "audit-selftest: verifying audit.sh actually detects violations"

# --- check 1: private source repo links --------------------------------------
check fail private-repo-link \
  '<a href="https://github.com/defiantsoftware/clowder">source</a>'
check fail private-repo-subpath \
  '<a href="https://github.com/defiantsoftware/clowder/issues/1">issue</a>'

# The two public repos share a prefix with the private one. If these trip the
# guard, the pattern lost its trailing boundary class and the build breaks on
# legitimate links.
check pass public-tap-link \
  '<a href="https://github.com/defiantsoftware/homebrew-clowder">tap</a>'
check pass public-site-link \
  '<a href="https://github.com/defiantsoftware/clowder-site">site</a>'

# --- check 2: stale base-path prefixes ---------------------------------------
check fail stale-base-src  '<img src="/clowder-site/favicon.svg">'
check fail stale-base-href '<link href="/clowder-site/style.css">'

# Root-absolute paths are CORRECT on the apex domain — this is the invariant
# that inverted when the site left the /clowder-site project subpath.
check pass root-absolute-asset '<img src="/favicon.svg"><link href="/style.css">'

# --- a wholly clean page must pass -------------------------------------------
check pass clean-page \
  '<a href="https://github.com/defiantsoftware/homebrew-clowder">tap</a><img src="/favicon.svg">'

# --- check 3: private-source leakage -----------------------------------------
check_file fail leaked-rust    'main.rs'
check_file fail leaked-swift   'App.swift'
check_file fail leaked-cargo   'Cargo.toml'
check_file fail leaked-script  'build-app.sh'
check_file fail leaked-plist   'Info.plist'

# Files the site legitimately publishes must not trip it. .webmanifest and .xml in particular are
# real build outputs (manifest.webmanifest.ts and the sitemap integration).
check_file pass site-manifest  'manifest.webmanifest'
check_file pass site-sitemap   'sitemap-index.xml'
check_file pass site-css       'style.css'

check fail leaked-token-name \
  '<script>const t = "HOMEBREW_TAP_TOKEN";</script>'
check fail leaked-rust-symbol \
  '<p>uses clowder_proto internally</p>'

# The product NAME is obviously fine — only the internal symbols are not.
check pass product-name-is-fine \
  '<h1>Clowder</h1><p>a terminal for coding agents</p>'

echo "audit-selftest: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
