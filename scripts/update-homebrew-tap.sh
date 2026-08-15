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

# The site is the primary channel for these — it reads the same files locally — but the tap release
# is the only public record independent of it.
NOTES="$ROOT/site/src/content/releases/v$VERSION.md"
BODY="$(mktemp)"; trap 'rm -f "$BODY"' EXIT

if [ -f "$NOTES" ]; then
  "$ROOT/scripts/check-release-notes.sh" --file "$NOTES" >/dev/null
  # Strip the frontmatter block; publish the prose.
  awk 'BEGIN{n=0} /^---[[:space:]]*$/{n++; next} n>=2' "$NOTES" > "$BODY"
else
  # DELIBERATELY NOT FATAL. This runs at step 17 of the release job — AFTER step 14 tags the commit
  # and step 15 publishes the GitHub Release. Aborting here would strand a signed, notarized,
  # tagged, published release with no installable artifact on the tap, over a missing markdown
  # file. Enforcement belongs in the bump job, where failing costs nothing.
  echo "::warning::no release notes at $NOTES — publishing the stub body"
  printf 'Clowder %s\n' "$VERSION" > "$BODY"
fi

# 1. Host the DMG on the PUBLIC tap's Releases (so brew can fetch it unauthenticated). Idempotent:
#    create the release if missing, then upload/replace the asset — and set the notes on BOTH paths,
#    not just at create time, so a re-run (e.g. after Step 2's cask push failed) still picks up notes
#    that did not exist yet on the first pass.
echo "==> Uploading DMG to $TAP_REPO ($TAG)"
if ! gh release view "$TAG" --repo "$TAP_REPO" >/dev/null 2>&1; then
  gh release create "$TAG" --repo "$TAP_REPO" --title "$TAG" --notes-file "$BODY"
else
  gh release edit "$TAG" --repo "$TAP_REPO" --notes-file "$BODY"
fi
gh release upload "$TAG" "$DMG" --repo "$TAP_REPO" --clobber

# 2. Render + push the cask (sha256 is hex, version is dotted digits — both safe as sed replacements).
CASK="$(sed -e "s/@@VERSION@@/$VERSION/" -e "s/@@SHA256@@/$SHA/" "$TEMPLATE")"

WORK="$(mktemp -d)"
# Extend, not replace, the EXIT trap set above for $BODY — a second `trap ... EXIT` overwrites the
# first rather than stacking, so without this the notes tempfile set up before Step 1 would leak.
trap 'rm -rf "$WORK"; rm -f "$BODY"' EXIT
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
