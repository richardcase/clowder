/**
 * Site-wide constants.
 *
 * clowder is public and Apache-2.0 licensed. Public-facing install links go to the Homebrew tap;
 * there is deliberately no separate "view source" link on the marketing site itself.
 */

export const site = {
  name: 'Clowder',
  tagline: 'Run a fleet of coding agents. Never lose track of one.',
  description:
    'Clowder is a native macOS terminal that orchestrates Claude Code, Codex and shell agents — each isolated in its own git worktree — and tells you which one needs your attention.',
  url: 'https://getclowder.app',
  tapRepo: 'https://github.com/richardcase/homebrew-clowder',
  installCmd: 'brew install --cask richardcase/clowder/clowder',
  upgradeCmd: 'brew upgrade --cask clowder',
  uninstallCmd: 'brew uninstall --cask clowder',
  /** Matches LSMinimumSystemVersion in the shipped 0.6.0 Info.plist. */
  minMacOS: '14',
  minMacOSName: 'Sonoma',
  /** Verified with `lipo -archs` against the shipped 0.6.0 binaries. */
  arch: 'Apple silicon',
  copyrightHolder: 'Richard Case',
} as const;

/** Join a public/ asset path onto the configured GitHub Pages base path. */
export function asset(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, '');
  return `${base}/${path.replace(/^\//, '')}`;
}
