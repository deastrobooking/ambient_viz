// Offline preprocessing: parse irocz.svg, flatten cubic Béziers into
// polylines, normalize to a centered unit shape, and emit a JS constant.
//
// The SVG has a single <path> inside a <g transform="translate(...)">. We
// apply that translate to absolute commands. Relative commands keep their
// deltas as-is.

const fs = require('fs');
const SRC = 'irocz.svg';
const OUT = 'silhouette.js';
const FLATTEN_TOL = 1.0; // user-units (mm) — coarser = fewer points

const svg = fs.readFileSync(SRC, 'utf8');

const pathMatch = svg.match(/<path\b[^>]*\bd="([^"]+)"/s);
if (!pathMatch) throw new Error('No <path d="..."> found');
const d = pathMatch[1];

let tx = 0, ty = 0;
const trMatch = svg.match(/transform="translate\(\s*([-\d.eE+]+)\s*,\s*([-\d.eE+]+)\s*\)"/);
if (trMatch) { tx = parseFloat(trMatch[1]); ty = parseFloat(trMatch[2]); }
console.log(`translate: ${tx}, ${ty}`);

// --- tokenize ---
function tokenize(str) {
  const out = [];
  let i = 0;
  while (i < str.length) {
    const c = str[i];
    if (/[a-zA-Z]/.test(c)) { out.push(c); i++; continue; }
    if (/[\s,]/.test(c)) { i++; continue; }
    // number
    let j = i;
    if (str[j] === '-' || str[j] === '+') j++;
    let sawDot = false, sawE = false;
    while (j < str.length) {
      const ch = str[j];
      if (ch >= '0' && ch <= '9') { j++; continue; }
      if (ch === '.' && !sawDot && !sawE) { sawDot = true; j++; continue; }
      if ((ch === 'e' || ch === 'E') && !sawE) { sawE = true; j++; if (str[j] === '+' || str[j] === '-') j++; continue; }
      break;
    }
    if (j === i) { i++; continue; } // skip unknown
    out.push(parseFloat(str.slice(i, j)));
    i = j;
  }
  return out;
}

const tokens = tokenize(d);

// --- walk + flatten ---
const subpaths = [];
let currentSub = null;
let curX = 0, curY = 0;
let startX = 0, startY = 0;
let prevCtrlX = null, prevCtrlY = null;

function pushPoint(x, y) {
  if (!currentSub) currentSub = [];
  // dedupe consecutive identical points
  if (currentSub.length) {
    const [lx, ly] = currentSub[currentSub.length - 1];
    if (Math.abs(lx - x) < 1e-6 && Math.abs(ly - y) < 1e-6) return;
  }
  currentSub.push([x, y]);
}

function startSub(x, y) {
  if (currentSub && currentSub.length) subpaths.push(currentSub);
  currentSub = [];
  pushPoint(x, y);
}

function flattenCubic(x0, y0, x1, y1, x2, y2, x3, y3, tol) {
  const stack = [[x0, y0, x1, y1, x2, y2, x3, y3]];
  while (stack.length) {
    const [a0, b0, a1, b1, a2, b2, a3, b3] = stack.pop();
    const dx = a3 - a0, dy = b3 - b0;
    const lenSq = dx * dx + dy * dy;
    let dist;
    if (lenSq < 1e-18) {
      const e1 = Math.hypot(a1 - a0, b1 - b0);
      const e2 = Math.hypot(a2 - a0, b2 - b0);
      dist = Math.max(e1, e2);
    } else {
      // perpendicular distance from control points to the chord (a0,b0)-(a3,b3)
      const len = Math.sqrt(lenSq);
      const d1 = Math.abs((dy * a1 - dx * b1 + a3 * b0 - b3 * a0) / len);
      const d2 = Math.abs((dy * a2 - dx * b2 + a3 * b0 - b3 * a0) / len);
      dist = Math.max(d1, d2);
    }
    if (dist <= tol) {
      pushPoint(a3, b3);
    } else {
      const m01x = (a0 + a1) / 2, m01y = (b0 + b1) / 2;
      const m12x = (a1 + a2) / 2, m12y = (b1 + b2) / 2;
      const m23x = (a2 + a3) / 2, m23y = (b2 + b3) / 2;
      const m012x = (m01x + m12x) / 2, m012y = (m01y + m12y) / 2;
      const m123x = (m12x + m23x) / 2, m123y = (m12y + m23y) / 2;
      const mx = (m012x + m123x) / 2, my = (m012y + m123y) / 2;
      stack.push([mx, my, m123x, m123y, m23x, m23y, a3, b3]);
      stack.push([a0, b0, m01x, m01y, m012x, m012y, mx, my]);
    }
  }
}

let i = 0;
let cmd = null;
function num() { return tokens[i++]; }

while (i < tokens.length) {
  if (typeof tokens[i] === 'string') cmd = tokens[i++];
  // implicit repeat uses last cmd; M/m -> L/l after first pair
  let executedCmd = cmd;
  switch (cmd) {
    case 'M': {
      const x = num() + tx, y = num() + ty;
      curX = x; curY = y; startX = x; startY = y;
      startSub(x, y);
      cmd = 'L';
      break;
    }
    case 'm': {
      const dx = num(), dy = num();
      // first 'm' acts as 'M' (per SVG spec) — but the only place this matters
      // is at the beginning of d, where we treat it as moveto absolute. Since
      // the tokenize stream begins with 'M' here, treat 'm' uniformly as relative.
      curX += dx; curY += dy;
      startX = curX; startY = curY;
      startSub(curX, curY);
      cmd = 'l';
      break;
    }
    case 'L': {
      curX = num() + tx; curY = num() + ty;
      pushPoint(curX, curY);
      break;
    }
    case 'l': {
      curX += num(); curY += num();
      pushPoint(curX, curY);
      break;
    }
    case 'H': { curX = num() + tx; pushPoint(curX, curY); break; }
    case 'h': { curX += num(); pushPoint(curX, curY); break; }
    case 'V': { curY = num() + ty; pushPoint(curX, curY); break; }
    case 'v': { curY += num(); pushPoint(curX, curY); break; }
    case 'C': {
      const x1 = num() + tx, y1 = num() + ty;
      const x2 = num() + tx, y2 = num() + ty;
      const x3 = num() + tx, y3 = num() + ty;
      flattenCubic(curX, curY, x1, y1, x2, y2, x3, y3, FLATTEN_TOL);
      prevCtrlX = x2; prevCtrlY = y2;
      curX = x3; curY = y3;
      break;
    }
    case 'c': {
      const x1 = curX + num(), y1 = curY + num();
      const x2 = curX + num(), y2 = curY + num();
      const x3 = curX + num(), y3 = curY + num();
      flattenCubic(curX, curY, x1, y1, x2, y2, x3, y3, FLATTEN_TOL);
      prevCtrlX = x2; prevCtrlY = y2;
      curX = x3; curY = y3;
      break;
    }
    case 'S': {
      const x1 = prevCtrlX !== null ? 2 * curX - prevCtrlX : curX;
      const y1 = prevCtrlY !== null ? 2 * curY - prevCtrlY : curY;
      const x2 = num() + tx, y2 = num() + ty;
      const x3 = num() + tx, y3 = num() + ty;
      flattenCubic(curX, curY, x1, y1, x2, y2, x3, y3, FLATTEN_TOL);
      prevCtrlX = x2; prevCtrlY = y2;
      curX = x3; curY = y3;
      break;
    }
    case 's': {
      const x1 = prevCtrlX !== null ? 2 * curX - prevCtrlX : curX;
      const y1 = prevCtrlY !== null ? 2 * curY - prevCtrlY : curY;
      const x2 = curX + num(), y2 = curY + num();
      const x3 = curX + num(), y3 = curY + num();
      flattenCubic(curX, curY, x1, y1, x2, y2, x3, y3, FLATTEN_TOL);
      prevCtrlX = x2; prevCtrlY = y2;
      curX = x3; curY = y3;
      break;
    }
    case 'Z': case 'z': {
      curX = startX; curY = startY;
      if (currentSub && currentSub.length) {
        subpaths.push(currentSub);
        currentSub = null;
      }
      cmd = null;
      executedCmd = 'Z';
      break;
    }
    default:
      throw new Error('Unhandled command: ' + cmd);
  }
  // reset tangent reflection unless we just did C/c/S/s
  if (!['C','c','S','s'].includes(executedCmd)) {
    prevCtrlX = null; prevCtrlY = null;
  }
}
if (currentSub && currentSub.length) subpaths.push(currentSub);

console.log(`${subpaths.length} subpaths after flatten`);

// --- normalize ---
let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
for (const sp of subpaths) {
  for (const [x, y] of sp) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
}
const cx = (minX + maxX) / 2;
const cy = (minY + maxY) / 2;
const w = maxX - minX;
const h = maxY - minY;
const scale = 1 / Math.max(w, h);
console.log(`bbox ${w.toFixed(3)} x ${h.toFixed(3)} aspect=${(w/h).toFixed(4)}`);

const norm = subpaths.map(sp => sp.map(([x, y]) => [
  +((x - cx) * scale).toFixed(4),
  +((y - cy) * scale).toFixed(4),
]));

const total = norm.reduce((a, sp) => a + sp.length, 0);
console.log(`${total} total points`);

// --- emit ---
const lines = [];
lines.push('// auto-generated from irocz.svg — do not hand-edit');
lines.push(`// regenerate: node preprocess.js`);
lines.push(`// ${norm.length} subpaths, ${total} points; flatten tolerance ${FLATTEN_TOL} user-units`);
lines.push(`const CAR_ASPECT = ${(w/h).toFixed(4)};`);
lines.push('const CAR_SUBPATHS = [');
for (const sp of norm) {
  // emit as a single packed array of x,y,x,y,... to keep the file small
  const flat = sp.map(p => p.join(',')).join(',');
  lines.push(`  [${flat}],`);
}
lines.push('];');
fs.writeFileSync(OUT, lines.join('\n') + '\n');
console.log(`wrote ${OUT}`);
