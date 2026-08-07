# shellcheck shell=bash
# The Conventional Commits grammar — the single source of truth for this repo's commit convention.
#
# Sourced by scripts/check-commit-messages.sh (lint) and scripts/next-version.sh (semver). Those two
# MUST agree: a subject the linter accepts has to be parseable into a version bump, and a type the
# bumper honours has to be one the linter allows. Keeping one pattern here is what guarantees it.
#
# Not executable: source it, don't run it.

# Scope is free-form on purpose: milestone scopes (`m10c`) and comma-joined scopes (`proto,daemon`)
# are both already in use. A trailing `!` marks a breaking change.
CC_TYPES='feat|fix|docs|test|refactor|perf|ci|chore|build|style|revert'

# Capture groups: 1 = type, 3 = scope (without parens), 4 = "!" or empty, 5 = description.
# Anchored at both ends so a trailing-newline or multi-line subject cannot sneak through.
CC_PATTERN="^(${CC_TYPES})(\(([^)]+)\))?(!)?: (.+)$"

# Returns 0 if the subject conforms. Reverts produced by GitHub's revert button (`Revert "…"`) are
# exempt — that subject is not ours to shape. `fixup!`/`squash!` are deliberately NOT exempt: an
# unsquashed fixup would land in main forever.
cc_subject_ok() {
  case "$1" in
    'Revert "'*) return 0 ;;
  esac
  # The right-hand side of =~ must stay UNQUOTED or bash matches it as a literal string.
  [[ $1 =~ $CC_PATTERN ]]
}

# Parse a subject into CC_TYPE / CC_SCOPE / CC_BREAKING (0|1) / CC_DESC. Returns 1 if it does not
# parse — which includes the `Revert "…"` exemption above: a revert carries no type, so it
# contributes no version bump. Callers that only care about validity want cc_subject_ok instead.
cc_parse() {
  CC_TYPE=''
  CC_SCOPE=''
  CC_BREAKING=0
  CC_DESC=''
  [[ $1 =~ $CC_PATTERN ]] || return 1
  CC_TYPE="${BASH_REMATCH[1]}"
  CC_SCOPE="${BASH_REMATCH[3]}"
  if [ -n "${BASH_REMATCH[4]}" ]; then CC_BREAKING=1; fi
  CC_DESC="${BASH_REMATCH[5]}"
}
