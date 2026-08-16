# site

Marketing site for [Clowder](https://getclowder.app) — a cross-platform
agent-orchestrator terminal for macOS.

Built with [Astro](https://astro.build) and deployed to GitHub Pages.

This site lives inside the private `clowder` repo under `site/`. It has its own `package.json` and
CI job and needs no Rust, Swift or libghostty toolchain. The two rules below are enforced by
`scripts/audit.sh`, which also refuses to publish product source files or private marker strings —
see `scripts/audit-selftest.sh` for exactly what that means.

## Develop

```sh
npm ci
npm run dev      # http://localhost:4321
npm run check    # type-check .astro and .ts — `astro build` does NOT type-check
npm run build    # → dist/, then scripts/audit.sh
npm test         # scripts/audit-selftest.sh
```

`npm run check` is worth running before pushing. **`astro build` does not type-check** — it
strips types rather than checking them, and exits 0 on a type error — so `check` is the only thing
that catches one. CI runs it on every pull request.

## How it stays current

`src/data/release.ts` reads the latest release from the **public Homebrew tap**
(`defiantsoftware/homebrew-clowder`) at build time and exports the version, the direct `.dmg` URL and
its size. A daily `schedule:` trigger in
[`.github/workflows/deploy-site.yml`](../.github/workflows/deploy-site.yml) — at the repo root, not
in this directory — rebuilds the site, so a new Clowder release propagates here without anyone
editing a file.

If the GitHub API call fails, the build falls back to a pinned version and logs a warning rather
than failing the deploy.

## Two rules for anyone editing this directory

1. **Never link to `github.com/defiantsoftware/clowder`.** That repository is private — the link
   404s for every visitor. Public-facing links go to the Homebrew tap instead.
2. **Never hardcode a `/clowder-site/` path prefix.** The site moved from a GitHub Pages project
   subpath to the apex domain `getclowder.app`, so it is now served from the root and any surviving
   base-path prefix 404s. Reference public assets through the `asset()` helper in
   `src/data/site.ts`, which derives the prefix from Astro's `BASE_URL`.

Both are checked by `scripts/audit.sh`, which runs against `dist/` after a build.

## Deploying

Pushes to `main` that touch `site/**` deploy automatically. The repo's **Settings ▸ Pages ▸ Source**
must be set to **GitHub Actions** for the first deploy to publish.

## Screenshots

`src/assets/screenshots/` holds real captures of Clowder 0.5.0, rendered by
`src/components/AppShot.astro` through Astro's asset pipeline (hashed, WebP, responsive `srcset`,
base path handled automatically).

The captures already include the app's own window chrome, so `AppShot` deliberately does **not**
wrap them in a CSS window frame.

To refresh them, run **`site/scripts/capture-screenshots.sh`** from a Terminal window you opened
yourself — it sizes the window to 1720×900, captures each shot at 2×, and downscales to 2400px. It
must be your own GUI session: a process with no window-server connection can launch Clowder but the
app never creates a window, so there is nothing to capture.

The script only sizes the window and takes the picture; you arrange the app between shots. It
captures the window rectangle alone, so nothing else on your desktop can end up in the file.

Two things to get right before you start, both of which the script reminds you about:

- **Claude Code's welcome banner shows the signed-in account's name, email and plan.** Use a
  `shell` adapter pane, which cannot render it at all. Scrolling the banner out of frame also
  works, but it depends on you noticing it every time — and an email address that reaches the live
  site is not fixable by editing the image afterwards, because the image has already shipped.
- **The sidebar lists every registered project.** Unregister anything private with
  `clowder project rm <path>` before capturing, and re-add it afterwards.

## Icons

`public/favicon.svg` is the source of truth for the mark. The raster fallbacks are generated
from it:

```sh
node scripts/make-icons.mjs   # apple-touch-icon.png, favicon.ico, icon-{192,512}.png
```

Edit `favicon.svg` and re-run — never hand-edit the outputs, or they drift from the mark and
nobody can tell which is current. `manifest.webmanifest` is a route
(`src/pages/manifest.webmanifest.ts`) so its name and description come from `src/data/site.ts`
rather than being a second copy.

The touch and manifest icons are cropped to the mark's own rounded rect so it bleeds to every
edge. iOS and Android apply their own mask, and leaving the SVG's padding in would produce a
teal tile floating inside a dark one.

## Social card

`public/og.png` is the 1200×630 Open Graph card, generated from `fleet.png` and the site's own
tokens by:

```sh
node scripts/make-og-image.mjs
```

Regenerate it whenever the tagline or `fleet.png` changes — nothing rebuilds it automatically, and
a stale card is invisible from the site itself. It is deliberately *not* wired into `npm run build`:
the output is static, and adding an image pipeline to every build would be cost without benefit.

The script draws its text as SVG for sharp to rasterise, so it must stick to widely-present font
families. **A missing font renders nothing rather than erroring** — check the output after changing
any `font-family`.

After changing the card, re-scrape it: platforms cache aggressively, so the old image will keep
appearing until you force a refresh through the relevant validator.
