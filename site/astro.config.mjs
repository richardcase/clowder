import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

// Deployed to GitHub Pages on the custom apex domain getclowder.app, so the
// site is served from the root and needs no `base`. (It was previously a
// *project* site under /clowder-site, which is why the audit script still
// checks that no stale base-path prefixes survive in the build.)
export default defineConfig({
  site: 'https://getclowder.app',
  trailingSlash: 'ignore',
  // Reads `site` above for absolute URLs, so it needs no configuration of its
  // own. `filter` keeps 404.astro out: it is a real route as far as the build is
  // concerned, but listing an error page for indexing is exactly wrong.
  integrations: [sitemap({ filter: (page) => !page.includes('/404') })],
  build: {
    inlineStylesheets: 'always',
  },
});
