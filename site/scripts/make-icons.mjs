// Regenerates the raster icon set from public/favicon.svg.
//
//   node scripts/make-icons.mjs
//
// Writes apple-touch-icon.png, favicon.ico and the two manifest icons. Committed
// as a generator for the same reason as make-og-image.mjs: hand-made binaries
// drift from the mark they were cut from and nobody can tell which is current.
// Not wired into `npm run build` — the output only changes when favicon.svg does.
//
// favicon.svg stays the primary icon; everything here is a fallback for clients
// that do not read SVG favicons, iOS home screens chief among them.
import sharp from 'sharp';
import { writeFile } from 'node:fs/promises';

const SRC = 'public/favicon.svg';
const BG = '#080d0f'; // --bg, matching the theme-color meta tag

/**
 * Render the mark at `size`, optionally on an opaque background.
 *
 * `density` is scaled with the target so the SVG rasterises at full resolution
 * rather than being drawn at 72dpi and upscaled — without it the 512px icon comes
 * out visibly soft.
 */
async function render(size, { opaque = false } = {}) {
  let img = sharp(SRC, { density: Math.max(72, Math.ceil((size / 100) * 72)) }).resize(size, size);
  if (opaque) img = img.flatten({ background: BG }).removeAlpha();
  return img.png({ compressionLevel: 9 }).toBuffer();
}

/**
 * The mark, cropped to its own rounded rect so it bleeds to every edge.
 *
 * favicon.svg draws an 84-unit rect inset by 8 in a 100-unit viewBox. Home-screen
 * icons get masked to a squircle by the OS, so leaving that padding in produces a
 * teal tile floating inside a dark one — an icon inside an icon. Cropping to the
 * rect means the OS mask lands roughly where the rect's own radius already is,
 * and the result reads as the same mark the favicon shows.
 *
 * Done by cropping rather than by drawing a second full-bleed SVG, so favicon.svg
 * stays the only place the mark is defined.
 */
async function renderBleed(size) {
  const inset = 0.08, scale = 1 / (1 - inset * 2); // 100/84
  const full = Math.round(size * scale);
  return sharp(SRC, { density: Math.max(72, Math.ceil((full / 100) * 72)) })
    .resize(full, full)
    .extract({
      left: Math.round(full * inset),
      top: Math.round(full * inset),
      width: size,
      height: size,
    })
    .flatten({ background: BG })
    .removeAlpha()
    .png({ compressionLevel: 9 })
    .toBuffer();
}

/**
 * Pack PNGs into an ICO container.
 *
 * ICO can embed PNG data directly rather than the legacy BMP form, which every
 * browser worth supporting has read for well over a decade. Hand-building the
 * 22-byte header beats taking a dependency for it.
 */
function buildIco(images) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(images.length, 4);

  let offset = 6 + images.length * 16;
  const entries = images.map(({ size, data }) => {
    const e = Buffer.alloc(16);
    e.writeUInt8(size >= 256 ? 0 : size, 0); // 0 encodes 256
    e.writeUInt8(size >= 256 ? 0 : size, 1);
    e.writeUInt8(0, 2); // palette size — 0 for truecolour
    e.writeUInt8(0, 3); // reserved
    e.writeUInt16LE(1, 4); // colour planes
    e.writeUInt16LE(32, 6); // bits per pixel
    e.writeUInt32LE(data.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += data.length;
    return e;
  });

  return Buffer.concat([header, ...entries, ...images.map((i) => i.data)]);
}

// iOS ignores SVG favicons and composites the tile onto white, so a transparent
// mark disappears entirely. Opaque, full-bleed, at the size iOS asks for.
await writeFile('public/apple-touch-icon.png', await renderBleed(180));

// Transparent, so the mark sits on whatever chrome the browser puts behind it.
// 48px included for Windows taskbar/shortcut use, which reaches past 32.
const ico = [16, 32, 48];
await writeFile(
  'public/favicon.ico',
  buildIco(await Promise.all(ico.map(async (size) => ({ size, data: await render(size) }))))
);

// Manifest icons get the same full-bleed treatment: Android applies its own mask
// to adaptive icons, so the padding would compound the same way.
for (const size of [192, 512]) {
  await writeFile(`public/icon-${size}.png`, await renderBleed(size));
}

console.log('wrote apple-touch-icon.png, favicon.ico, icon-192.png, icon-512.png');
