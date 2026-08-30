#!/usr/bin/env bash
# Bump the cask on the PUBLIC Homebrew tap. The DMG itself is NOT touched here — it already lives on
# clowder's own GitHub Release (release.yml's "Publish GitHub Release" step attaches it before this
# script ever runs), and the cask template points straight at that. See docs/homebrew.md for why this
# used to re-host the DMG on the tap and no longer does. Called by release.yml on a final
# (non-pre-release) signed release.
#
# Env:
#   VERSION              (default: the repo VERSION file)
#   DMG                  (default: dist/Clowder-<VERSION>-macos.dmg) — read locally only to compute
#                         its sha256 for the cask; never uploaded from here.
#   TAP_REPO             owner/name of the tap repo (default: richardcase/homebrew-tap)
#   HOMEBREW_TAP_TOKEN   REQUIRED — fine-grained PAT with contents:write on the tap repo. Used to push
#                        the cask commit (https).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION")}"
DMG="${DMG:-$ROOT/dist/Clowder-$VERSION-macos.dmg}"
TAP_REPO="${TAP_REPO:-richardcase/homebrew-tap}"
TEMPLATE="$ROOT/scripts/homebrew/clowder.rb.tmpl"

# Validate before deriving anything from it. This script is documented as manually runnable, takes
# VERSION from the environment, and every artefact below is built by string-substitution — so a
# malformed value propagates silently rather than failing.
#
# That is not hypothetical. Run once with VERSION=v0.7.0 (leading `v`), it produced a `vv0.7.0` tap
# tag, a cask reading `version "v0.7.0"`, and a download URL pointing at `Clowder-v0.7.0-macos.dmg`
# while the uploaded asset was `Clowder-0.7.0-macos.dmg` — a live 404 on `brew install --cask`.
#
# The pattern is copied verbatim from scripts/set-version.sh so the two cannot disagree about what a
# version is: whatever set-version.sh will write to VERSION, this must accept, and nothing else.
if ! [[ $VERSION =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: VERSION '$VERSION' is not X.Y.Z[-prerelease]" >&2
  echo "hint: pass the bare version, not the tag — VERSION=0.7.0, not VERSION=v0.7.0" >&2
  exit 1
fi

[ -n "${HOMEBREW_TAP_TOKEN:-}" ] || { echo "HOMEBREW_TAP_TOKEN is required" >&2; exit 1; }
[ -f "$DMG" ]      || { echo "no DMG at: $DMG" >&2; exit 1; }
[ -f "$TEMPLATE" ] || { echo "no cask template at: $TEMPLATE" >&2; exit 1; }

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
echo "==> clowder $VERSION  sha256=$SHA"

# Render + push the cask (sha256 is hex, version is dotted digits — both safe as sed replacements).
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
echo "==> Published cask ($VERSION) to $TAP_REPO"
