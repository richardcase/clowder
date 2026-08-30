#!/usr/bin/env bash
# Publish the DMG + cask to the PUBLIC Homebrew tap. The DMG is (re)hosted on the tap repo's
# Releases rather than the cask pointing at the source repo's own Releases — see docs/homebrew.md
# for why. Called by release.yml on a final (non-pre-release) signed release.
#
# Env:
#   VERSION              (default: the repo VERSION file)
#   DMG                  (default: dist/Clowder-<VERSION>-macos.dmg)
#   TAP_REPO             owner/name of the tap repo (default: richardcase/homebrew-clowder)
#   HOMEBREW_TAP_TOKEN   REQUIRED — fine-grained PAT with contents:write on the tap repo. Used for both
#                        the `gh` release upload and the cask `git push` (https).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION")}"
DMG="${DMG:-$ROOT/dist/Clowder-$VERSION-macos.dmg}"
TAP_REPO="${TAP_REPO:-richardcase/homebrew-clowder}"
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

# DELIBERATELY NOT FATAL, in every branch below — missing file, guard-rejected file, or a file that
# yields nothing once frontmatter is stripped. This runs at step 17 of the release job — AFTER
# step 14 tags the commit and step 15 publishes the GitHub Release. Aborting here for ANY of those
# reasons would strand a signed, notarized, tagged, published release with no installable artifact
# on the tap, over a markdown file. Enforcement (rejecting bad notes, requiring a fragment at all)
# belongs in the bump job, where failing costs nothing; here it can only warn and fall back to the
# stub body. A guard-rejected file is a real problem someone must go chase — hence the distinct
# warning text below — just not one worth destroying a published release over.
if [ ! -f "$NOTES" ]; then
  echo "::warning::no release notes at $NOTES — publishing the stub body"
  printf 'Clowder %s\n' "$VERSION" > "$BODY"
elif ! "$ROOT/scripts/check-release-notes.sh" --file "$NOTES" >/dev/null 2>&1; then
  echo "::warning::release notes at $NOTES failed the content guard — publishing the stub body instead"
  printf 'Clowder %s\n' "$VERSION" > "$BODY"
else
  # Strip the frontmatter block; publish the prose. `n<2` on the match guards the counter so a bare
  # `---` markdown horizontal rule *inside* the body (after the closing frontmatter fence) is no
  # longer mistaken for a third frontmatter delimiter and silently dropped — it used to count every
  # `---`-only line unconditionally.
  awk 'n<2 && /^---[[:space:]]*$/{n++; next} n>=2' "$NOTES" > "$BODY"
  if [ ! -s "$BODY" ]; then
    # Guard passed but nothing survived frontmatter-stripping (e.g. no frontmatter fence at all, so
    # the n>=2 gate never opens) — publish the stub rather than a silently empty release body.
    echo "::warning::release notes at $NOTES produced an empty body after stripping frontmatter — publishing the stub body instead"
    printf 'Clowder %s\n' "$VERSION" > "$BODY"
  fi
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
