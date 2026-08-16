#!/usr/bin/env bash
#
# Capture the three marketing screenshots from a running Clowder.app.
#
# This encodes the recipe that used to live only in site/README.md prose. It exists because the
# captures have to be redone every few releases and the fiddly parts — the exact window size, the 2x
# capture, the `sips -Z 2400` downscale — are easy to get subtly wrong, and wrong is only visible
# once the images are on the live site.
#
# It must be run from YOUR OWN GUI session (a Terminal window you opened), not from an automation
# context: a process without a window-server connection can launch Clowder but the app will never
# create a window, so there is nothing to capture.
#
# Usage: site/scripts/capture-screenshots.sh [out-dir]      (default: site/src/assets/screenshots)
#
# WHAT YOU MUST DO BEFORE RUNNING
#
#   1. Unregister private projects. The sidebar lists every registered project by name.
#        clowder project rm <path>          (re-add them afterwards)
#
#   2. Use `shell` panes, not `claude` panes. Claude Code's welcome banner prints the signed-in
#      account's NAME, EMAIL and PLAN. Keeping it out of frame by scrolling is fragile — a shell
#      pane cannot render it at all. This is the single most important rule here: an email address
#      on a public marketing site is not fixable by editing the image afterwards, because the image
#      has already shipped.
set -euo pipefail

APP='Clowder'
W=1720
H=900
TARGET_WIDTH=2400   # final asset width; the README and AppShot.astro both assume it

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/site/src/assets/screenshots}"

die() { echo "error: $*" >&2; exit 1; }

command -v screencapture >/dev/null || die "screencapture not found (macOS only)"
command -v sips          >/dev/null || die "sips not found (macOS only)"
[ -d "$OUT" ] || die "no such output directory: $OUT"

pgrep -f "$APP.app/Contents/MacOS/clowder-app" >/dev/null \
  || die "$APP is not running — open it from Finder or the Dock first, and make sure a window is visible"

# ---------------------------------------------------------------- window geometry

# Ask System Events rather than assuming: the app may be on a secondary display, and a window that
# refuses to resize (some layouts have a minimum) must fail loudly rather than silently producing
# assets at the wrong aspect ratio.
osa() { osascript -e "tell application \"System Events\" to tell process \"$APP\" $1"; }

osa "to set position of window 1 to {80, 80}" >/dev/null 2>&1 \
  || die "could not position the window — grant Terminal accessibility access in System Settings > Privacy & Security > Accessibility"
osa "to set size of window 1 to {$W, $H}" >/dev/null

got="$(osa 'to get size of window 1')"
want="$W, $H"
[ "$got" = "$want" ] || die "window is ${got// /} but must be ${want// /} — resize it by hand and re-run"

read -r x y <<<"$(osa 'to get position of window 1' | tr -d ',')"

echo "window: ${W}x${H} at ${x},${y}"
echo

# ---------------------------------------------------------------- capture

# shot <name> <instruction>
shot() {
  # Split across two `local`s on purpose: within a single `local`, a later assignment cannot see an
  # earlier one, so `raw="$OUT/$name.png"` would silently expand $name to nothing (shellcheck SC2318).
  local name="$1" instruction="$2"
  local raw="$OUT/$name.png"
  echo "── $name"
  echo "   $instruction"
  printf '   press RETURN when the window looks right (or s to skip): '
  read -r reply </dev/tty
  case "$reply" in
    s | S) echo "   skipped"; return 0 ;;
  esac

  # -R captures exactly the window rect, so nothing else on your desktop is ever in the file.
  # -x silences the shutter sound. On a Retina display this yields a 2x image automatically.
  screencapture -x -R"${x},${y},${W},${H}" "$raw"

  local px
  px="$(sips -g pixelWidth "$raw" | awk '/pixelWidth/{print $2}')"
  if [ "$px" -lt "$TARGET_WIDTH" ]; then
    echo "   WARNING: captured at ${px}px wide, below the ${TARGET_WIDTH}px target." >&2
    echo "   A non-Retina display cannot produce these assets at full quality." >&2
  fi

  sips -Z "$TARGET_WIDTH" "$raw" >/dev/null
  echo "   wrote $raw ($(sips -g pixelWidth -g pixelHeight "$raw" | awk '/pixel/{printf "%s ", $2}')px)"
  echo
}

cat <<'EOF'
Three shots. Between each one, arrange the app yourself — this script only sizes the window and
takes the picture.

EOF

shot fleet   "Sidebar showing several agents, one pane focused with some real output in it."
shot palette "Same, with the command palette open (⌘K)."
shot split   "Same, with the pane split (⌘D) so two panes are side by side."

# ---------------------------------------------------------------- after

cat <<EOF
Done. Before committing, look at each image and confirm none of these is visible:

  - an email address or account name (Claude Code's banner)
  - a private project name in the sidebar
  - a filesystem path you would rather not publish

Then:
  cd $ROOT/site && npm run build     # the audit runs against dist/
  # and update the "captures of Clowder X.Y.Z" line in site/README.md
EOF
