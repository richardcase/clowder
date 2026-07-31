#!/usr/bin/env bash
# Set the clowder version everywhere from a single source. Writes the top-level VERSION file and the
# Cargo workspace version, then refreshes Cargo.lock. The macOS bundle version flows from VERSION via
# scripts/build-app.sh (Info.plist), and SwiftPM versioning comes from the git tag.
#
# Usage: scripts/set-version.sh <X.Y.Z>   (or no arg to re-propagate the current VERSION)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "$#" -ge 1 ]; then
  VERSION="$1"
else
  VERSION="$(tr -d '[:space:]' < VERSION)"
fi

# Validate semver X.Y.Z (optionally a -prerelease suffix).
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) : ;;
  *) echo "error: version '$VERSION' is not X.Y.Z" >&2; exit 1 ;;
esac

echo "==> Setting version $VERSION"
printf '%s\n' "$VERSION" > VERSION

# Update [workspace.package] version in the root Cargo.toml (the single Rust version source).
# Only the line inside the [workspace.package] section is touched.
awk -v v="$VERSION" '
  /^\[workspace\.package\]/ { inwp=1 }
  inwp && /^\[/ && !/^\[workspace\.package\]/ { inwp=0 }
  inwp && /^version[[:space:]]*=/ { print "version = \"" v "\""; next }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

echo "==> Refreshing Cargo.lock"
( source "$HOME/.cargo/env" 2>/dev/null || true; cargo update --workspace >/dev/null )

echo "==> Version is now $VERSION"
grep '^version' <(awk '/^\[workspace\.package\]/{f=1} f&&/^version/{print;exit}' Cargo.toml)
