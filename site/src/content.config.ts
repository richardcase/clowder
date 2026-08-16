import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

// Only *.md at this exact directory — NOT `**/*.md`. In-flight fragments live in the sibling
// src/content/unreleased/ and must never render as if they had shipped. The sibling layout is what
// makes that structural rather than a property of this pattern, but keep the pattern narrow anyway.
const releases = defineCollection({
  loader: glob({ base: './src/content/releases', pattern: '*.md' }),
  schema: z.object({
    /** Semver without the leading `v`, e.g. `0.6.0`. Sorted numerically, never as a string. */
    version: z.string(),
    /** ISO date, e.g. `2026-08-12`. */
    date: z.string(),
  }),
});

export const collections = { releases };
