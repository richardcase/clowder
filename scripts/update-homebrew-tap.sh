#!/usr/bin/env bash
# Render the Homebrew cask for the current VERSION + the built DMG's sha256 and push it to the tap repo
# over SSH (using a write deploy key). Called by release.yml on a final (non-pre-release) signed release.
#
# Env:
#   VERSION                  (default: the repo VERSION file)
#   DMG                      (default: dist/Clowder-<VERSION>-macos.dmg)
#   TAP_REPO                 owner/name of the tap repo (default: richardcase/homebrew-clowder)
#   HOMEBREW_TAP_DEPLOY_KEY  REQUIRED — the tap repo's write deploy key (private key, PEM text)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION")}"
DMG="${DMG:-$ROOT/dist/Clowder-$VERSION-macos.dmg}"
TAP_REPO="${TAP_REPO:-richardcase/homebrew-clowder}"
TEMPLATE="$ROOT/scripts/homebrew/clowder.rb.tmpl"

[ -n "${HOMEBREW_TAP_DEPLOY_KEY:-}" ] || { echo "HOMEBREW_TAP_DEPLOY_KEY is required" >&2; exit 1; }
[ -f "$DMG" ]      || { echo "no DMG at: $DMG" >&2; exit 1; }
[ -f "$TEMPLATE" ] || { echo "no cask template at: $TEMPLATE" >&2; exit 1; }

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
echo "==> clowder $VERSION  sha256=$SHA"

# Render the cask (sha256 is hex, version is dotted digits — both safe as sed replacements).
CASK="$(sed -e "s/@@VERSION@@/$VERSION/" -e "s/@@SHA256@@/$SHA/" "$TEMPLATE")"

# Isolated SSH using the deploy key; accept github.com's host key on first use (no known_hosts prep).
WORK="$(mktemp -d)"
KEYFILE="$(mktemp)"
cleanup() { rm -rf "$WORK" "$KEYFILE"; }
trap cleanup EXIT
printf '%s\n' "$HOMEBREW_TAP_DEPLOY_KEY" > "$KEYFILE"
chmod 600 "$KEYFILE"
export GIT_SSH_COMMAND="ssh -i $KEYFILE -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"

echo "==> Cloning $TAP_REPO"
git clone --depth 1 "git@github.com:$TAP_REPO.git" "$WORK"

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
echo "==> Pushed Casks/clowder.rb ($VERSION) to $TAP_REPO"
