// Quick verification: render the silhouette subpaths to a PPM so we can
// confirm the SVG flattening produced the right shape.
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const src = fs.readFileSync(path.join(__dirname, 'silhouette.js'), 'utf8');
const ctx = {};
new Function('exports', src.replace(/const (\w+)/g, 'exports.$1'))(ctx);
const { CAR_ASPECT, CAR_SUBPATHS } = ctx;

const W = 600;
const H = Math.round(W / CAR_ASPECT);

// odd-even fill via scanline
const filled = new Uint8Array(W * H);
function fillSubpath(pts) {
  // pts is flat array of x, y pairs (normalized to roughly [-0.5, 0.5])
  // convert to canvas pixel coords
  const px = [];
  for (let i = 0; i < pts.length; i += 2) {
    px.push([
      (pts[i] + 0.5 / CAR_ASPECT) * W * CAR_ASPECT, // map x from [-w/2, w/2] of normalized space to [0, W]
      (pts[i + 1] + 0.5 / CAR_ASPECT) * W * CAR_ASPECT // same scale for y to keep aspect
    ]);
  }
  // Actually let me just do straight: x=(nx+0.5*aspect)*scale, y=(ny+0.5)*scale where scale = H
  // since longest axis was scaled to 1, and aspect = w/h means w=aspect*h, so normalized x range is [-aspect/2, aspect/2] when h is the longer axis.
  // Hmm CAR_ASPECT > 1 so w is the longer axis -> normalized x in [-0.5, 0.5], y in [-0.5/aspect, 0.5/aspect]
  // Map to pixel space: x' = (nx + 0.5) * W; y' = (ny + 0.5/aspect) * W  (since H = W / aspect)
  px.length = 0;
  for (let i = 0; i < pts.length; i += 2) {
    px.push([
      (pts[i] + 0.5) * W,
      (pts[i + 1] + 0.5 / CAR_ASPECT) * W,
    ]);
  }

  // even-odd scanline fill on this single subpath: toggle pixels along each scanline
  // We accumulate intersections per row then xor-fill spans.
  // For a complete car: simpler to just fill pixel-by-pixel with point-in-polygon.
  // But we have 47 subpaths and want even-odd across all. Do that outside.
}

// Combined even-odd fill across all subpaths.
function pointInSubpath(x, y, pts) {
  let inside = false;
  const n = pts.length / 2;
  for (let i = 0, j = n - 1; i < n; j = i++) {
    const xi = (pts[i*2] + 0.5) * W;
    const yi = (pts[i*2+1] + 0.5 / CAR_ASPECT) * W;
    const xj = (pts[j*2] + 0.5) * W;
    const yj = (pts[j*2+1] + 0.5 / CAR_ASPECT) * W;
    if (((yi > y) !== (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi)) inside = !inside;
  }
  return inside;
}

for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    let count = 0;
    for (const sp of CAR_SUBPATHS) {
      if (pointInSubpath(x + 0.5, y + 0.5, sp)) count++;
    }
    filled[y * W + x] = (count & 1) ? 0 : 255; // even-odd: odd = inside (black)
  }
}

const header = `P5\n${W} ${H}\n255\n`;
fs.writeFileSync('/tmp/verify.pgm', Buffer.concat([Buffer.from(header), Buffer.from(filled)]));
execSync('magick /tmp/verify.pgm /tmp/verify.png');
console.log('Wrote /tmp/verify.png');
