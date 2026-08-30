#!/usr/bin/env bash
#
# Post-build guards for failure modes that are invisible locally but break the
# live site. Wired into `npm run build`, so a violation fails the deploy
# rather than shipping.
#
#   1. Stale `/clowder-site` base-path prefixes — the site now serves from the
#      root of getclowder.app, so a surviving prefix 404s in production.
#
# This check is itself covered by scripts/audit-selftest.sh. That is not
# ceremony: a guard whose failure mode is "silently passes" is worse than no
# guard, because it reports `ok` while checking nothing. (A prior version of
# this file also guarded against links to the clowder source repo while it was
# private; clowder is public and Apache-2.0 licensed now, so that check was
# removed rather than left checking nothing.)
set -euo pipefail

DIST="${1:-dist}"
status=0

if [[ ! -d "$DIST" ]]; then
  echo "audit: no '$DIST' directory — run 'astro build' first" >&2
  exit 1
fi

echo "audit: checking $DIST"

# --- 1. stale base-path prefixes ---------------------------------------------
# The inverse of the check this replaced. While the site was a GitHub Pages
# *project* site every absolute path had to carry the `/clowder-site` base; on
# the apex domain a root-absolute path is correct and the base prefix is the
# bug.
matches=$(grep -rIoE '(src|href)="/clowder-site[^"]*' "$DIST" || true)
if [[ -n "$matches" ]]; then
  echo "audit: FAIL — stale '/clowder-site' base path (the site serves from the root):" >&2
  echo "$matches" | sort -u >&2
  status=1
else
  echo "  ok  no stale '/clowder-site' base-path prefixes"
fi

# --- 2. build-secret / source leakage ----------------------------------------
# The site lives inside the same monorepo as the product source and CI secrets, so a bad import
# or a build-time file read could publish something that was never meant to be public — the repo
# being open source doesn't make a leaked runtime secret value harmless. dist/ carries no
# provenance, so this checks for what leaked source would look like rather than where it came from.
#
# Milestone 2 makes site.ts read scripts/lib/product.sh at build time — a deliberate read from
# outside site/. This guard exists so the next one is not silently broader.
leaked=$(find "$DIST" -type f \
  \( -name '*.rs' -o -name '*.swift' -o -name '*.toml' -o -name '*.lock' \
     -o -name '*.plist' -o -name '*.sh' -o -name '*.a' -o -name '*.entitlements' \) || true)
if [[ -n "$leaked" ]]; then
  echo "audit: FAIL — product source files in the published site:" >&2
  echo "$leaked" | sort -u >&2
  status=1
else
  echo "  ok  no product source files in the build"
fi

# Marker strings that only appear in the private tree or in CI configuration. A published page
# containing any of these means something read further than it should have.
markers='HOMEBREW_TAP_TOKEN|DOPPLER_TOKEN|APPLE_ID_PASSWORD|clowder_proto|ghostty_surface_'
matches=$(grep -rIoE "$markers" "$DIST" || true)
if [[ -n "$matches" ]]; then
  echo "audit: FAIL — private markers in the published site:" >&2
  echo "$matches" | sort -u >&2
  status=1
else
  echo "  ok  no private source markers in the build"
fi

if [[ $status -eq 0 ]]; then
  echo "audit: passed"
fi

exit $status
