/**
 * Numeric, not lexicographic, semver comparison: string order puts `0.9.0` above `0.10.0`.
 *
 * Shared by whats-new.astro (the full changelog, which sorts every collected release) and
 * WhatsNew.astro (the homepage teaser, which both sorts and filters down to the newest
 * downloadable one). Both used to carry their own copy of this; extracted here so the two can't
 * drift.
 */

const key = (v: string) => v.split('.').map(Number);

/** Descending comparator — newest first — for `Array.prototype.sort`. */
export const cmp = (a: string, b: string): number => {
  const [x, y] = [key(a), key(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (y[i] ?? 0) - (x[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
};

/** True when `a` is a strictly newer version than `b`. */
export const isNewer = (a: string, b: string): boolean => cmp(a, b) < 0;
