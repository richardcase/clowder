/**
 * Site-wide constants.
 *
 * IMPORTANT: `github.com/defiantsoftware/clowder` is a PRIVATE repo. Linking to it
 * anywhere on this site gives every visitor a 404. All public-facing links go
 * to the public Homebrew tap instead. There is deliberately no "view source"
 * link, and the build asserts on this (see the private-link audit in the
 * verification steps).
 */

export const site = {
  name: 'Clowder',
  tagline: 'Run a fleet of coding agents. Never lose track of one.',
  description:
    'Clowder is a native macOS terminal that orchestrates Claude Code, Codex and shell agents — each isolated in its own git worktree — and tells you which one needs your attention.',
  url: 'https://getclowder.app',
  tapRepo: 'https://github.com/defiantsoftware/homebrew-clowder',
  installCmd: 'brew install --cask defiantsoftware/clowder/clowder',
  upgradeCmd: 'brew upgrade --cask clowder',
  uninstallCmd: 'brew uninstall --cask clowder',
  /** Matches LSMinimumSystemVersion in the shipped 0.6.0 Info.plist. */
  minMacOS: '14',
  minMacOSName: 'Sonoma',
  /** Verified with `lipo -archs` against the shipped 0.6.0 binaries. */
  arch: 'Apple silicon',
  copyrightHolder: 'Defiant Software',
} as const;

/** Join a public/ asset path onto the configured GitHub Pages base path. */
export function asset(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, '');
  return `${base}/${path.replace(/^\//, '')}`;
}
