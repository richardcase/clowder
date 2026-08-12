#!/usr/bin/env bash
# Publish the DMG + cask to the PUBLIC Homebrew tap. The clowder source repo is private, so its release
# assets aren't publicly downloadable — the DMG is (re)hosted on the tap repo's Releases, and the cask
# points there. Called by release.yml on a final (non-pre-release) signed release.
#
# Env:
#   VERSION              (default: the repo VERSION file)
#   DMG                  (default: dist/Clowder-<VERSION>-macos.dmg)
#   TAP_REPO             owner/name of the tap repo (default: defiantsoftware/homebrew-clowder)
#   HOMEBREW_TAP_TOKEN   REQUIRED — fine-grained PAT with contents:write on the tap repo. Used for both
#                        the `gh` release upload and the cask `git push` (https).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION")}"
DMG="${DMG:-$ROOT/dist/Clowder-$VERSION-macos.dmg}"
TAP_REPO="${TAP_REPO:-defiantsoftware/homebrew-clowder}"
TEMPLATE="$ROOT/scripts/homebrew/clowder.rb.tmpl"
TAG="v$VERSION"

[ -n "${HOMEBREW_TAP_TOKEN:-}" ] || { echo "HOMEBREW_TAP_TOKEN is required" >&2; exit 1; }
[ -f "$DMG" ]      || { echo "no DMG at: $DMG" >&2; exit 1; }
[ -f "$TEMPLATE" ] || { echo "no cask template at: $TEMPLATE" >&2; exit 1; }

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
echo "==> clowder $VERSION  sha256=$SHA"

export GH_TOKEN="$HOMEBREW_TAP_TOKEN"

# 1. Host the DMG on the PUBLIC tap's Releases (so brew can fetch it unauthenticated). Idempotent:
#    create the release if missing, then upload/replace the asset.
echo "==> Uploading DMG to $TAP_REPO ($TAG)"
if ! gh release view "$TAG" --repo "$TAP_REPO" >/dev/null 2>&1; then
  gh release create "$TAG" --repo "$TAP_REPO" --title "$TAG" --notes "Clowder $VERSION"
fi
gh release upload "$TAG" "$DMG" --repo "$TAP_REPO" --clobber

# 2. Render + push the cask (sha256 is hex, version is dotted digits — both safe as sed replacements).
CASK="$(sed -e "s/@@VERSION@@/$VERSION/" -e "s/@@SHA256@@/$SHA/" "$TEMPLATE")"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
git clone --depth 1 "https://x-access-token:${HOMEBREW_TAP_TOKEN}@github.com/${TAP_REPO}.git" "$WORK"

mkdir -p "$WORK/Casks"
printf '%s\n' "$CASK" > "$WORK/Casks/clowder.rb"

git -C "$WORK" config user.name  "clowder-release-bot"
git -C "$WORK" config user.email "clowder-release-bot@users.noreply.github.com"
git -C "$WORK" add Casks/clowder.rb
if git -C "$WORK" diff --cached --quiet; then
  echo "==> Cask already up to date ($VERSION) — nothing to push."
  exit 0
fi
git -C "$WORK" commit -m "clowder $VERSION"
git -C "$WORK" push origin HEAD:main
echo "==> Published DMG + cask ($VERSION) to $TAP_REPO"
