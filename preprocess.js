// Offline preprocessing: extract the outer silhouette of irocz.png as a
// simplified polygon, normalized to a centered unit shape, and emit a JS
// constant we can paste into index.html.
//
// Pipeline:
//   1. ImageMagick threshold to a raw 1-byte-per-pixel buffer (0 = car body,
//      255 = white).
//   2. Flood-fill from the canvas edges to mark "outside" — anything not
//      reachable from the edge through white pixels is part of the silhouette
//      (this fills in the windshield/headlight white pockets).
//   3. Find the topmost-leftmost foreground pixel; trace the outer contour
//      with Moore-Neighbor (8-connected).
//   4. Simplify with Douglas-Peucker.
//   5. Normalize to coordinates centered around (0,0) with longest axis = 1.
//   6. Write CAR_SILHOUETTE as a JS array literal.

const fs = require('fs');
const { execSync } = require('child_process');

const SRC = 'irocz.png';
const RAW = '/tmp/irocz_silhouette.raw';
const OUT = 'silhouette.js';
const THRESHOLD_PCT = 60;
const DP_EPSILON = 1.5;

const dim = execSync(`magick identify -format "%wx%h" ${SRC}`).toString().trim();
const [W, H] = dim.split('x').map(Number);
console.log(`Image: ${W}x${H}`);

execSync(`magick ${SRC} -colorspace Gray -threshold ${THRESHOLD_PCT}% gray:${RAW}`);
const raw = fs.readFileSync(RAW);
if (raw.length !== W * H) {
  throw new Error(`Expected ${W*H} bytes, got ${raw.length}`);
}

// Flood-fill outside via BFS from the canvas border.
const outside = new Uint8Array(W * H);
const queue = [];
function tryEnqueue(x, y) {
  if (x < 0 || x >= W || y < 0 || y >= H) return;
  const idx = y * W + x;
  if (outside[idx]) return;
  if (raw[idx] === 255) {
    outside[idx] = 1;
    queue.push(x, y);
  }
}
for (let x = 0; x < W; x++) { tryEnqueue(x, 0); tryEnqueue(x, H - 1); }
for (let y = 0; y < H; y++) { tryEnqueue(0, y); tryEnqueue(W - 1, y); }
while (queue.length) {
  const x = queue.shift();
  const y = queue.shift();
  tryEnqueue(x + 1, y);
  tryEnqueue(x - 1, y);
  tryEnqueue(x, y + 1);
  tryEnqueue(x, y - 1);
}

// Silhouette mask: 1 where the car (including filled-in white pockets) is.
const sil = new Uint8Array(W * H);
for (let i = 0; i < W * H; i++) sil[i] = outside[i] ? 0 : 1;

// Find topmost-leftmost foreground pixel as the starting point.
let startX = -1, startY = -1;
outer: for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    if (sil[y * W + x]) { startX = x; startY = y; break outer; }
  }
}
if (startX < 0) throw new Error('No silhouette found');
console.log(`Start: (${startX}, ${startY})`);

// Moore-Neighbor contour tracing (8-connected, clockwise).
// Directions indexed CW from East: 0=E, 1=SE, 2=S, 3=SW, 4=W, 5=NW, 6=N, 7=NE.
const dirs = [
  [1, 0], [1, 1], [0, 1], [-1, 1],
  [-1, 0], [-1, -1], [0, -1], [1, -1],
];
function isFg(x, y) {
  return x >= 0 && x < W && y >= 0 && y < H && sil[y * W + x] === 1;
}

const path = [[startX, startY]];
let cx = startX, cy = startY;
// We entered the start pixel from the west (since it's topmost-leftmost),
// so the previous-step direction is east. Begin search from one CW step
// past the back-direction, i.e. from north going CW.
let prevDir = 0;
const MAX_ITER = W * H * 4;
for (let iter = 0; iter < MAX_ITER; iter++) {
  // Search starts at the neighbor "behind us" (opposite of how we entered),
  // then proceeds clockwise.
  const searchStart = (prevDir + 5) % 8;
  let found = false;
  for (let i = 0; i < 8; i++) {
    const d = (searchStart + i) % 8;
    const [dx, dy] = dirs[d];
    const nx = cx + dx, ny = cy + dy;
    if (isFg(nx, ny)) {
      cx = nx; cy = ny;
      prevDir = d;
      found = true;
      break;
    }
  }
  if (!found) break;
  if (cx === startX && cy === startY) break;
  path.push([cx, cy]);
}
console.log(`Contour: ${path.length} points`);

// Douglas-Peucker simplify.
function perpDist(p, a, b) {
  const dx = b[0] - a[0], dy = b[1] - a[1];
  const lenSq = dx * dx + dy * dy;
  if (lenSq === 0) {
    const ex = p[0] - a[0], ey = p[1] - a[1];
    return Math.sqrt(ex * ex + ey * ey);
  }
  const t = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / lenSq;
  const px = a[0] + t * dx, py = a[1] + t * dy;
  const ex = p[0] - px, ey = p[1] - py;
  return Math.sqrt(ex * ex + ey * ey);
}
function dp(points, eps) {
  if (points.length < 3) return [...points];
  let maxD = 0, maxI = 0;
  const a = points[0], b = points[points.length - 1];
  for (let i = 1; i < points.length - 1; i++) {
    const d = perpDist(points[i], a, b);
    if (d > maxD) { maxD = d; maxI = i; }
  }
  if (maxD > eps) {
    const left = dp(points.slice(0, maxI + 1), eps);
    const right = dp(points.slice(maxI), eps);
    return left.slice(0, -1).concat(right);
  }
  return [a, b];
}
const simplified = dp(path, DP_EPSILON);
console.log(`Simplified (eps=${DP_EPSILON}): ${simplified.length} points`);

// Normalize: center around origin, scale longest axis to 1.
const xs = simplified.map(p => p[0]);
const ys = simplified.map(p => p[1]);
const minX = Math.min(...xs), maxX = Math.max(...xs);
const minY = Math.min(...ys), maxY = Math.max(...ys);
const cxN = (minX + maxX) / 2;
const cyN = (minY + maxY) / 2;
const sw = maxX - minX;
const sh = maxY - minY;
const scale = 1 / Math.max(sw, sh);
console.log(`bbox ${sw}x${sh} aspect=${(sw/sh).toFixed(3)}`);

const norm = simplified.map(p => [
  +((p[0] - cxN) * scale).toFixed(4),
  +((p[1] - cyN) * scale).toFixed(4),
]);

const lines = [];
lines.push('// auto-generated from irocz.png — do not hand-edit');
lines.push('// regenerate via: node preprocess.js');
lines.push(`// ${norm.length} points; aspect (w/h) ≈ ${(sw/sh).toFixed(3)}`);
lines.push(`const CAR_ASPECT = ${(sw/sh).toFixed(4)};`);
lines.push('const CAR_SILHOUETTE = [');
for (const [x, y] of norm) {
  lines.push(`  [${x},${y}],`);
}
lines.push('];');

fs.writeFileSync(OUT, lines.join('\n') + '\n');
console.log(`Wrote ${OUT}`);
