import type { APIRoute } from 'astro';
import { site } from '../data/site';

/**
 * Served as a route rather than a static file in public/ so the name and
 * description come from src/data/site.ts, the same way the JSON-LD block does,
 * instead of being a second copy that quietly drifts.
 *
 * `display: "browser"` is deliberate. This is a marketing page, not an app;
 * claiming "standalone" would strip the browser chrome from something that is
 * only ever a single page with outbound links.
 *
 * Icons are declared `purpose: "any"` and not "maskable". Maskable icons must
 * keep their content inside a 80% safe zone, and promising that without testing
 * the mask on device is how marks end up cropped.
 */
export const GET: APIRoute = () =>
  new Response(
    JSON.stringify(
      {
        name: site.name,
        short_name: site.name,
        description: site.description,
        start_url: '/',
        display: 'browser',
        theme_color: '#080d0f',
        background_color: '#080d0f',
        icons: [
          { src: '/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
          { src: '/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
        ],
      },
      null,
      2
    ),
    { headers: { 'Content-Type': 'application/manifest+json' } }
  );
