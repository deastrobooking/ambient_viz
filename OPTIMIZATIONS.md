# Performance Optimizations

Audit of `index.html` render loop, ordered by expected impact. Line numbers
reference the state at the time of writing; treat as approximate.

Existing performance notes live in `README.md` ("Performance notes"). This
document is a working list of *potential* optimizations not yet applied.

## Status legend

- `[ ]` not yet implemented
- `[x]` implemented (with notes on the actual change)

## Tier 1 — Big wins (likely 2× headroom or more)

### 1. Dither pass: move threshold to WebGL fragment shader — `[x]` done

**Diagnosis.** The old `applyDither` did a full GPU→CPU→GPU round trip
every frame: downsample to a CSS-resolution 2D canvas, `getImageData`
(allocates a fresh `Uint8ClampedArray` of `dw*dh*4` bytes and forces a
GPU sync), threshold every pixel in JS against the Bayer 8x8 LUT,
`putImageData`, then `drawImage` back to main at full resolution. README
measured this at ~10-20ms/frame.

**Implementation.**
- The dither canvas is now a WebGL1 canvas (variable name `ditherCanvas`
  preserved so the mono-region consumer downstream needs no changes).
  Context options: `preserveDrawingBuffer: true` (the mono-region pass
  reads it later in the same frame), `alpha/depth/stencil/antialias:
  false`, `premultipliedAlpha: false`.
- One-time at startup: compile a fullscreen-quad vertex shader plus a
  fragment shader that samples the source texture's R channel and
  thresholds against an 8×8 `LUMINANCE` Bayer LUT texture (pre-baked with
  `(BAYER8 * 4 + 2)`, the same threshold the CPU path used). 4-vertex
  triangle-strip quad in a `STATIC_DRAW` buffer. Source texture uses
  `LINEAR` filtering (matches the previous `imageSmoothingEnabled=true`
  downsample); Bayer texture uses `NEAREST` + `REPEAT`.
- Per frame: `texImage2D(canvas)` to upload the main 2D canvas to the
  source texture (browsers fast-path canvas-as-source uploads),
  `useProgram` + bind quad + bind both textures + set uniforms
  (`uTexSize`, `uOffset`), `drawArrays(TRIANGLE_STRIP, 0, 4)`, then
  `ctx.drawImage(ditherCanvas, ..., canvas.width, canvas.height)` to
  upscale onto the main canvas at device resolution with
  `imageSmoothingEnabled = false`.
- The Bayer phase animation (the slow per-frame drift, accelerated by
  treble) is unchanged — it just feeds `uOffset` instead of an inner-
  loop XOR.

**Caveats.**
- WebGL initialization throws if unavailable. WebGL1 is universally
  supported in modern browsers, but software-rendered/disabled-GPU edge
  cases would fail. No CPU fallback is retained.
- `gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true)` is required at upload
  time. Without it, the texture orientation is Y-inverted — invisible
  for the full-canvas upscale (lattice / flyouts / sparks are roughly
  Y-symmetric in distribution), but breaks the mono-region and
  inversion-region passes: clipped slice rects would repaint with
  content from the opposite Y of the canvas, reading as high-contrast
  intrusions rather than local re-tints. (This was caught in testing
  after the initial port.)
- The Bayer pattern's per-row phase along Y advances opposite the CPU
  path's direction. Visually invisible — the pattern is a continuously
  animated stochastic-looking texture either way; the starting phase
  differs. If bit-exact parity with the CPU path is ever wanted, flip
  the Y term in the shader's Bayer index.
- `texImage2D(..., canvas)` per frame (rather than `texSubImage2D`) is
  marginally heavier than necessary. If profiler shows it matters,
  switch to `texSubImage2D` after the initial upload.
- `preserveDrawingBuffer: true` carries a small per-frame cost (browser
  may keep an extra copy for compositor consistency) but guarantees the
  WebGL canvas is readable when the mono-region pass calls
  `drawImage(ditherCanvas, ...)` later in the same frame.

### 2. Flyout silhouettes: cache `Path2D` per shape — `[x]` done

**Diagnosis.** Each frame, for each of 10 flyouts, the code walked every
subpath of the active silhouette and emitted `moveTo`/`lineTo` per point —
for the car silhouette that's 47 subpaths × dozens of points each × 10
shapes ≈ thousands of path commands per frame, all to redraw fixed
geometry at varying scale/rotation.

**Implementation.**
- Right after the `SILHOUETTES` registry is built, a loop constructs a
  `Path2D` in normalized unit coords (`[-0.5, 0.5]`) for each entry and
  attaches it as `entry.path`. One-time cost at startup.
- The flyout draw loop replaces the inner `beginPath()` + nested
  `moveTo`/`lineTo`/`closePath` block with `ctx.scale(drawScale,
  drawScale)` and direct `ctx.fill(path, 'evenodd')` /
  `ctx.stroke(path)` calls.
- Pixel-space line widths are divided by `drawScale` (precomputed
  `invScale = 1 / drawScale`) so strokes still render at the intended CSS
  pixel widths despite the scaled transform.

**Caveats.**
- Strokes now go through the scaled transform. With non-uniform aspect
  ratios this could distort, but `ctx.scale(drawScale, drawScale)` is
  uniform — strokes remain circular.
- `Path2D` is well supported in all modern browsers. No fallback needed.
- The unused `aspect` field on each silhouette is left in place (not
  read by render code; would be a separate cleanup).

### 3. Lattice stamp loop: integer coords + branch on jitter==0
Lattice stamp loop (~2794-2798).

At `LATTICE_SPACING=24` on 1600×900, ~4000 `ctx.drawImage(particleCanvas,
x, y)` calls per frame at sub-pixel coords.

- `Math.random()` runs twice per stamp even when `smoothTreble` ≈ 0 (so
  `jitter` ≈ 0). Branch out the no-jitter path.
- `drawImage` to fractional coordinates forces filtering even with
  `imageSmoothingEnabled=false`. Use `(x|0)` and `(y|0)`. Visual
  difference is imperceptible at this density.

### 4. Grain: pre-bake a noise ring buffer
`regenNoise` (~1203-1214).

~200k `Math.random()` calls per frame to fill a 320×320 noise canvas.

Options:
- Build N=24 noise canvases at startup, advance the index per frame,
  `drawImage` the active one. Modulate intensity via `globalAlpha`
  rather than re-thresholding pixel-by-pixel.
- If true per-frame noise is required, fill a `Uint8Array` with
  `crypto.getRandomValues` (one bulk call, much faster than N
  individual `Math.random()`s) and threshold from that.

## Tier 2 — Solid mid-tier wins

### 5. Scanlines: replace per-row `fillRect` loop with a tiled pattern — `[x]` done

**Diagnosis.** The original loop ran ~`H / SCANLINE_PERIOD` `fillRect`
calls per frame (≈450 at 1600×900, `SCANLINE_PERIOD=2`). Each row was
one paint op against the heavy translucent black `fillStyle` — easy to
collapse into a single tiled fill.

**Implementation.**
- A 1×`SCANLINE_PERIOD` offscreen canvas (`scanlineTile`) is built once
  at startup with row 0 fully opaque black and remaining rows
  transparent.
- A `CanvasPattern` (`scanlinePattern = ctx.createPattern(scanlineTile,
  'repeat')`) is created once and stored.
- Per frame: one `ctx.fillRect(0, 0, W, H)` with `fillStyle =
  scanlinePattern`, gated by `ctx.globalAlpha = SCANLINE_ALPHA`.
  `globalAlpha` is reset to `1` after.
- Using `globalAlpha` for intensity (instead of baking the alpha into
  the tile) means the live `SCANLINE_ALPHA` slider does not require
  re-baking the tile.

**Caveats.**
- `SCANLINE_PERIOD` is currently `const`. If it ever becomes runtime-
  tunable, the tile and pattern must be rebuilt when it changes.
- Patterns are subject to the current transform. The main canvas is
  drawn under `setTransform(dpr, 0, 0, dpr, 0, 0)`, so the 1×2 tile
  scales to 1×2 CSS pixels (= 2×4 device pixels at dpr=2) — matching
  the original behavior of `fillRect(0, y, W, 1)` in CSS-pixel space.

### 6. Sparks: stop allocating `createRadialGradient` per spark per frame
Sparks loop (~2752-2758).

Each visible spark allocates a fresh `CanvasGradient` and re-parses color
stops every frame. Build a single radial-gradient sprite
(white→transparent on a 64×64 offscreen) once at startup. Stamp via
`drawImage` with `globalAlpha = 0.9 * a` and scale to `radius * 5`.

### 7. Slice tears: stop using canvas-to-self `drawImage`
Slice loop (~2829-2832).

Per spec the browser must snapshot the source before drawing when source
and destination are the same canvas. Every slice forces this; the
rotated variant adds a transformed copy on top.

Maintain a `mirrorCanvas`. Once per frame, after the lattice pass,
`drawImage(canvas → mirrorCanvas)`. Then draw all slices from the mirror
into the main canvas. One snapshot per frame instead of N.

### 8. `lattPts` should be a `Float32Array`, not `[]`
Declared at ~1046, populated in `pushFilledRectPoints` (~1360), consumed
at ~2772-2798.

Currently a JS Array; `length = 0` keeps capacity, but `out.push(x, y)`
in the inner loop still triggers V8 element-kind transitions and slow
growth paths. Pre-size a `Float32Array` to the maximum point count for
the viewport at resize time, track a count cursor, write directly.

### 9. `globalAlpha` instead of `rgba(...)` template strings
Flyout (~2726, 2729, 2732), sparks (~2753-2754), and similar.

Each frame emits multiple fresh template-literal strings per shape;
each forces a CSS color parse on assignment to `fillStyle`/`strokeStyle`.
Set the color string once with the base RGB, modulate with `globalAlpha`.

## Tier 3 — Structural / smaller

### 10. Trim `ctx.save()`/`restore()` pairs
Flyout loop, slice loop, mono/inversion region loops.

Each pair has nontrivial cost. Most can be replaced by an explicit
`setTransform(dpr, 0, 0, dpr, 0, 0)` + `globalCompositeOperation =
'source-over'` reset at the section boundary.

### 11. `bands()` returns a fresh object every frame
~764. Mutate a module-level singleton instead. Tiny per call but it
runs every frame.

### 12. Hoist invariants out of `pushFilledRectPoints`
~1378: `lx < -halfW * 1.6` recomputes `halfW * 1.6` per iteration.
`Math.sqrt(3) / 2` is a constant. Hoist both.

### 13. Stop toggling `imageSmoothingEnabled` per draw region
Set in mono-region loop (~2905-2907), dither path, etc. State changes
on a 2D context aren't free; cache the desired value and only flip on
change.

### 14. Block shuffle (rare; low priority)
`applyBlockShuffle` (~1339-1358) has the same canvas-to-self issue as
slices. Lower priority because it fires only on energy onsets. If
addressing item #7 with a mirror canvas, reuse the mirror here too.

## Worth considering separately

### OffscreenCanvas + worker
`canvas.transferControlToOffscreen()` allows moving the entire render
loop off the main thread. UI / audio analysis stay on main; render runs
in a worker. Eliminates main-thread jank from layout, timers, and UI
event work. Big rewrite, but it's the move if the editor and renderer
ever need to coexist smoothly under load.

## Suggested order of attack

If picking off items one at a time:

1. ~~**#2 (Path2D for flyouts)**~~ — done.
2. ~~**#5 (scanline pre-bake)**~~ — done.
3. ~~**#1 (WebGL dither)**~~ — done.
4. **#3 (lattice integer coords + jitter branch)** — small diff, broad
   benefit during quiet passages.
5. **#6 (spark gradient sprite)** — kills per-frame allocation noise.
