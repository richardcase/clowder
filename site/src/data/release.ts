/**
 * Latest-release facts, resolved at build time.
 *
 * The Clowder *source* repo (richardcase/clowder) is private, so its release
 * assets are not publicly downloadable. The signed + notarized DMG is
 * re-hosted on the public Homebrew tap repo so `brew` (and this site) can
 * fetch it unauthenticated. That tap is therefore the source of truth here.
 *
 * A daily scheduled rebuild keeps this current without anyone editing a file.
 *
 * Failure is handled differently depending on where the build runs:
 *
 * - **Locally**, a failed lookup falls back to the pinned values below and warns.
 *   A stale version number beats blocking someone working offline or on a plane.
 * - **In CI**, it fails the build. The fallback used to apply everywhere, and its
 *   only signal was a `console.warn` inside an otherwise-green run — which nobody
 *   reads. That made the failure mode silent and unbounded: if the tap API broke,
 *   the daily rebuild would pin an old version and advertise a possibly-dead
 *   download link indefinitely, green the whole time. A failed build leaves the
 *   previous site up untouched, which is the safer of the two bad outcomes.
 *
 * CI also verifies the resolved DMG URL actually resolves. A well-formed URL
 * pointing at a deleted asset is the same silent failure with extra steps.
 *
 * The lookup is authenticated when GITHUB_TOKEN is in the environment, which both
 * workflows provide. See fetchOnce for why that matters on a CI runner.
 */

const TAP_REPO = 'richardcase/homebrew-clowder';
const RELEASES_API = `https://api.github.com/repos/${TAP_REPO}/releases/latest`;

/**
 * Whether to treat a degraded build as fatal. GitHub Actions sets CI=true; the
 * `!== 'false'` guard is for runners that set it to something else truthy.
 */
const IS_CI = Boolean(process.env.CI) && process.env.CI !== 'false';

/** Last known-good values. Verified 2026-08-12. */
const FALLBACK = {
  version: '0.6.0',
  dmgUrl: `https://github.com/${TAP_REPO}/releases/download/v0.6.0/Clowder-0.6.0-macos.dmg`,
  dmgSizeMb: 21,
} as const;

export interface Release {
  /** Semver without the leading `v`, e.g. `0.6.0`. */
  version: string;
  /** Direct, unauthenticated download URL for the signed DMG. */
  dmgUrl: string;
  /** Rounded download size in MB, for the trust line under the CTA. */
  dmgSizeMb: number;
  /** True when the GitHub API call failed and FALLBACK was used. */
  stale: boolean;
}

let cached: Promise<Release> | undefined;

/** A failure worth trying again, as opposed to one that will fail identically. */
class TransientError extends Error {}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function fetchOnce(): Promise<Release> {
  // Authenticated in CI, anonymous locally. Anonymous api.github.com allows 60
  // requests/hour per IP, and Actions runner IPs are shared between tenants, so
  // an unauthenticated build hits 403s intermittently. Both workflows pass the
  // ambient GITHUB_TOKEN, which lifts this to 5000/hour. Reading a public repo's
  // releases needs no scopes, so the read-only token Dependabot pull requests
  // get is enough.
  const token = process.env.GITHUB_TOKEN;

  let res: Response;
  try {
    res = await fetch(RELEASES_API, {
      headers: {
        Accept: 'application/vnd.github+json',
        'User-Agent': 'clowder-site-build',
        ...(token && { Authorization: `Bearer ${token}` }),
      },
      signal: AbortSignal.timeout(10_000),
    });
  } catch (err) {
    // Network-level failure or timeout — nothing about it says "will fail again".
    throw new TransientError(err instanceof Error ? err.message : String(err));
  }

  if (!res.ok) {
    const msg = `GitHub API returned ${res.status}`;
    // 5xx is the API having a bad minute; 403/429 is rate limiting, which clears.
    // A 404 means the repo or release genuinely is not there, and retrying that
    // just spends 30 seconds arriving at the same answer.
    if (res.status >= 500 || res.status === 403 || res.status === 429) {
      throw new TransientError(msg);
    }
    throw new Error(msg);
  }

  const json = (await res.json()) as {
    tag_name?: string;
    assets?: { name: string; browser_download_url: string; size: number }[];
  };

  const version = json.tag_name?.replace(/^v/, '');
  const dmg = json.assets?.find((a) => a.name.endsWith('.dmg'));

  if (!version || !dmg) throw new Error('no tagged .dmg asset in latest release');

  return {
    version,
    dmgUrl: dmg.browser_download_url,
    dmgSizeMb: Math.round(dmg.size / 1_000_000),
    stale: false,
  };
}

async function fetchRelease(): Promise<Release> {
  // Retried because this now fails the build in CI, and a check that cries wolf
  // on a one-off blip gets muted — which would recreate the exact silent failure
  // this is meant to surface.
  const delays = [1_000, 3_000];
  let lastErr: unknown;

  for (let attempt = 0; attempt <= delays.length; attempt++) {
    try {
      return await fetchOnce();
    } catch (err) {
      lastErr = err;
      if (!(err instanceof TransientError) || attempt === delays.length) break;
      console.warn(
        `[release] ${err.message} — retrying in ${delays[attempt] / 1000}s ` +
          `(attempt ${attempt + 2}/${delays.length + 1})`,
      );
      await sleep(delays[attempt]);
    }
  }

  console.warn(
    `[release] Falling back to pinned v${FALLBACK.version} — could not read ${TAP_REPO}: ${
      lastErr instanceof Error ? lastErr.message : lastErr
    }`,
  );
  return { ...FALLBACK, stale: true };
}

/**
 * Confirm the advertised download actually exists. HEAD first because it is
 * free; some CDNs refuse it, so fall back to asking for a single byte rather
 * than pulling ~21 MB just to learn the object is there.
 *
 * A definitive 4xx returns immediately — that is the answer, not an accident.
 * Anything else gets one retry, for the same reason the release lookup does: this
 * blocks merges now, and a check that fails on a passing cloud blip gets muted.
 */
async function isDownloadable(url: string): Promise<{ ok: boolean; detail: string }> {
  let detail = 'unreachable';

  for (let attempt = 0; attempt < 2; attempt++) {
    if (attempt) await sleep(2_000);

    for (const init of [
      { method: 'HEAD' },
      { method: 'GET', headers: { Range: 'bytes=0-0' } },
    ] as const) {
      try {
        const res = await fetch(url, { ...init, signal: AbortSignal.timeout(15_000) });
        if (res.ok) return { ok: true, detail: String(res.status) };
        detail = `HTTP ${res.status}`;
        // The asset is genuinely absent. Retrying reaches the same 404 slower.
        if (res.status >= 400 && res.status < 500 && res.status !== 429) {
          return { ok: false, detail };
        }
      } catch (err) {
        detail = err instanceof Error ? err.message : String(err);
      }
    }
  }

  return { ok: false, detail };
}

/**
 * In CI, refuse to publish a build that would quietly advertise the wrong thing.
 * Local builds are left alone so working offline stays possible.
 */
async function assertPublishable(release: Release): Promise<void> {
  if (!IS_CI) return;

  if (release.stale) {
    throw new Error(
      `[release] Refusing to build: could not read the latest release from ${TAP_REPO}, ` +
        `and publishing would advertise the pinned v${FALLBACK.version} as current.\n` +
        `  The warning above has the underlying cause.\n` +
        `  The previously deployed site is untouched, so nothing is broken for visitors.\n` +
        `  If the tap really has changed shape, update FALLBACK in src/data/release.ts.`,
    );
  }

  const { ok, detail } = await isDownloadable(release.dmgUrl);
  if (!ok) {
    throw new Error(
      `[release] Refusing to build: v${release.version} resolved, but its download is ` +
        `not reachable (${detail}).\n  ${release.dmgUrl}\n` +
        `  Publishing this would put a dead download button on the site.`,
    );
  }
}

export function getRelease(): Promise<Release> {
  cached ??= fetchRelease().then(async (release) => {
    await assertPublishable(release);
    return release;
  });
  return cached;
}
