// Regenerates the Open Graph card at public/og.png.
//
//   node scripts/make-og-image.mjs [outfile]
//
// Committed as a generator rather than a hand-made binary so the card can be
// rebuilt when the tagline, version line or screenshot changes, instead of
// drifting out of date because nobody has the source file. Not wired into
// `npm run build`: the output is static and adding an image pipeline to every
// build would be cost without benefit.
//
// Text is drawn as SVG and rasterised by sharp. Fonts resolve through
// fontconfig, so this must stay on widely-present families — a missing font
// silently renders nothing rather than erroring.
import sharp from 'sharp';

const W = 1200, H = 630;
const BG = '#080d0f', TEXT = '#e8f0f2', DIM = '#a7bcc2', ACCENT = '#2dd4bf', DEEP = '#0d8c8c';
const FONT = "Helvetica, Arial, sans-serif";
const OUT = process.argv[2] ?? 'public/og.png';

// Crop tightly to the part that carries the idea: the agent sidebar with its
// status dots, plus enough of the diff to read as a terminal. The full capture
// is mostly empty terminal, which becomes grey mush at card size — and a card is
// often viewed at half these dimensions again.
const SW = 566, SH = 358;
const shot = await sharp('src/assets/screenshots/fleet.png')
  .extract({ left: 0, top: 0, width: 1240, height: 784 })
  .resize(SW, SH, { fit: 'fill' })
  .toBuffer();

// Rounded corners and a hairline border, matching the screenshot treatment on
// the site. Fully contained rather than bled off the edge: a bleed loses the
// corner radius on that side and reads as a rendering bug rather than a crop.
const r = 12;
const shotRounded = await sharp(shot)
  .composite([
    {
      input: Buffer.from(
        `<svg width="${SW}" height="${SH}"><rect width="${SW}" height="${SH}" rx="${r}" ry="${r}" fill="#fff"/></svg>`
      ),
      blend: 'dest-in',
    },
    {
      input: Buffer.from(
        `<svg width="${SW}" height="${SH}"><rect x="0.5" y="0.5" width="${SW - 1}" height="${SH - 1}" rx="${r}" ry="${r}" fill="none" stroke="#2d4952" stroke-width="1"/></svg>`
      ),
      blend: 'over',
    },
  ])
  .png()
  .toBuffer();

const bg = Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">
  <defs>
    <radialGradient id="glow" cx="74%" cy="20%" r="58%">
      <stop offset="0%" stop-color="${ACCENT}" stop-opacity="0.28"/>
      <stop offset="100%" stop-color="${ACCENT}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="${W}" height="${H}" fill="${BG}"/>
  <rect width="${W}" height="${H}" fill="url(#glow)"/>
  <rect x="0" y="${H - 6}" width="${W}" height="6" fill="${ACCENT}"/>
</svg>`);

const text = Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">
  <rect x="64" y="66" width="46" height="46" rx="12" fill="${DEEP}"/>
  <text x="87" y="90" fill="#fff" font-family="${FONT}" font-size="26" font-weight="700"
        text-anchor="middle" dominant-baseline="central">C</text>
  <text x="126" y="90" fill="${TEXT}" font-family="${FONT}" font-size="27" font-weight="600"
        dominant-baseline="central" letter-spacing="0.2">Clowder</text>

  <text x="64" y="205" fill="${TEXT}" font-family="${FONT}" font-size="54" font-weight="700">Run a fleet of</text>
  <text x="64" y="266" fill="${TEXT}" font-family="${FONT}" font-size="54" font-weight="700">coding agents.</text>
  <text x="64" y="336" fill="${ACCENT}" font-family="${FONT}" font-size="46" font-weight="600"
        font-style="italic">Never lose track</text>
  <text x="64" y="392" fill="${ACCENT}" font-family="${FONT}" font-size="46" font-weight="600"
        font-style="italic">of one.</text>

  <text x="64" y="486" fill="${DIM}" font-family="${FONT}" font-size="21">A native macOS terminal that runs</text>
  <text x="64" y="514" fill="${DIM}" font-family="${FONT}" font-size="21">Claude Code, Codex and shell agents.</text>

  <text x="64" y="566" fill="#6f878e" font-family="${FONT}" font-size="19" font-weight="600"
        letter-spacing="0.6">getclowder.app · macOS 14+ · Apple silicon</text>
</svg>`);

await sharp(bg)
  .composite([
    { input: shotRounded, left: 570, top: 136 },
    { input: text, left: 0, top: 0 },
  ])
  // Flattened deliberately. The canvas is opaque anyway, but shipping an alpha
  // channel in a social card invites clients that composite onto white to render
  // it differently from every other client.
  // `flatten` composites the transparency away but leaves a (now redundant)
  // alpha channel behind, so drop it explicitly.
  .flatten({ background: BG })
  .removeAlpha()
  .png({ compressionLevel: 9 })
  .toFile(OUT);

const meta = await sharp(OUT).metadata();
console.log(`wrote ${OUT} — ${meta.width}x${meta.height}`);
