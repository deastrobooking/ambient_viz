# Performance degrade options for low-power devices

Reference target: Raspberry Pi 4 Model B (VideoCore VI, single-channel
LPDDR4) running Chromium at 1080p. Same advice applies to other weak
GPUs, low-end ARM Chromebooks, etc.

The visualizer's per-frame budget at 1080p is dominated by **Canvas2D
fillrate** and **stamp count**, not WebGL. The dither pass is the only
WebGL work; everything else is software-rasterized 2D. The 8GB of RAM on
the Pi 4 is irrelevant — this workload is not memory-bound.

Items are grouped by approximate impact. Numbers in parens are the
default values in `index.html`. Many of these are already exposed as
live sliders in the dev panel (press `~` to open) and can be tuned at
runtime without code changes.

---

## Tier 1 — Big wins

### Cap render resolution to 720p (or lower)

The biggest single lever. Render bitmap = `floor(W * dpr) × floor(H * dpr)`.
Every full-screen pass (trail fade, scanline fill, grain stamp, dither
sample, freeze drawImage) scales linearly with pixel count.

- 1920×1080 → 1280×720 cuts fillrate **2.25×**
- 1920×1080 → 960×540 cuts fillrate **4×**

CSS sizing (`100vw × 100vh`) stays the same; the browser upscales the
bitmap. Look loses sharpness but the dither/scanline aesthetic absorbs
the upscale gracefully — arguably *more* CRT-like.

Already automated by `?lite` (renders at 720p).

To go further by hand: edit `main()`'s `resize()` to clamp `dpr` against
a target max bitmap, e.g. `min(devicePixelRatio||1, 2, 960/W, 540/H)`.

### Force `dpr = 1` even on HiDPI

If the Pi is driving a 4K or retina-class screen, the default `dpr` cap
is 2, which means a 3840×2160 bitmap = ~8M pixels. Pinning `dpr = 1`
cuts that to 2M.

`index.html:2282` and `index.html:2285`:
```js
let dpr = Math.min(window.devicePixelRatio || 1, 2);
```
→ change to `let dpr = 1;`

Subsumed by the 720p cap above when running `?lite`.

### Sparser lattice

`LATTICE_SPACING` (default `24`) controls hex-lattice particle spacing
in CSS pixels. Stamp count scales **quadratically** — going from 24 to
36 cuts ~4000 stamps/frame to ~1800 (2.25× fewer); going to 48 cuts to
~1000 (4× fewer).

`index.html:629`:
```js
let LATTICE_SPACING = 24;
```

Also live in the dev panel and as an automation lane. `?lite` sets this
to `36` at init.

Visual cost: lattice grain gets coarser. At `36` the field still reads
as a uniform texture; at `48`+ individual dots become legible as
"particles" rather than a screen-tone.

### Disable the 3D mesh layer

`MESH3D_COUNT` (default `2`) controls how many low-poly wireframe meshes
are rendered each frame. Each mesh runs a JS software rasterizer
(vertex transform + back-face cull + line/vertex draw) per frame. Two
meshes is moderate; zero removes the layer entirely.

`index.html:1289`:
```js
let MESH3D_COUNT = 2;
```
→ live slider `mesh3dCount`, or set to `0` in init.

Visual cost: the rotating wireframe shapes go away. Lattice + flyouts
+ slices are unaffected.

---

## Tier 2 — Solid mid-tier wins

### Reduce flyout count

`FLYOUT_COUNT` (default `10`) — how many car silhouettes are projected
each frame. Each one is a `Path2D` fill + halo stroke + crisp stroke.
Halving to `5` halves the silhouette drawing cost.

`index.html:528`:
```js
const FLYOUT_COUNT = 10;
```

Currently a `const` — would need to become `let` and get a live param
hook if runtime-tunable, OR just edit the constant and reload.

Visual cost: noticeably less density of flying shapes during energetic
passages. Bass response still drives the same throb/alpha behavior on
the remaining ones.

### Drop grain resolution + alpha

`GRAIN_RES` (default `320`) is the noise canvas size, regenerated every
frame (~200K `Math.random()` calls). Smaller = cheaper.

- `GRAIN_RES = 160` → 4× fewer pixels to fill (one-time + per-frame regen)
- `GRAIN_ALPHA = 0` (or low value) → grain is still composited but
  invisible. To skip the regen entirely you'd need a code change at
  `regenNoise` (~line 1203) to early-return when alpha is below some
  threshold.

`index.html:540` (`const`) and `index.html:619` (`let`, live param).

### Increase scanline period

`SCANLINE_PERIOD` (default `2`) — every Nth row gets darkened. The
implementation now uses a single tiled fillRect (optimization #5 in
`OPTIMIZATIONS.md`) so this is cheap, but going to `3` or `4`
incrementally reduces overdraw on the tile.

`index.html:541` — currently `const`. Change requires rebuilding
`scanlineTile`/`scanlinePattern` (see `rebuildScanlineTile`).

Visual cost: slightly less CRT-line density. Subtle.

### Reduce slice tear count

Slice tears are additive `drawImage(canvas → canvas)` ops. Each slice
forces a browser-side snapshot. `SLICE_BURST_SCALE` (live, default
`1.0`) scales burst sizes; setting it to `0.5` halves average tears per
trigger. `MID_SLICE_TRIGGER` and `SLICE_TRIGGER` can be raised to fire
bursts less often.

Both live in the dev panel.

Visual cost: less visual "shattering" on bass kicks. The freeze and
shuffle responses still fire.

### Disable freeze captures

Freeze costs one full-canvas `drawImage` to capture, then one per frame
to replay (cheap, GPU-fast). Setting `FREEZE_FRAMES_MAX = 4` (the
minimum permitted by the existing slider) reduces how long each freeze
holds. Setting `freezeMonoChance = 0` removes the mono-region freeze
variant.

Both live params.

---

## Tier 3 — Smaller / situational

### Lower-resolution dither

The WebGL dither pass samples at CSS resolution and upscales. Halving
its working size cuts shader work 4×.

`resizeDither()` (~line 1571) sets the dither canvas size to `W × H`.
Change to `Math.ceil(W/2) × Math.ceil(H/2)` for a coarser, chunkier
dither. The README already lists this as the first optimization to try
if the dither pass becomes the bottleneck again.

Visual cost: dither pattern becomes obviously chunky. Some users
*prefer* this look — it's more 80s-VFD than fine-grained CRT.

### Drop sparks

Sparks allocate a fresh `CanvasGradient` per spark per frame
(`OPTIMIZATIONS.md` item #6). Setting a hard cap or skipping the spark
loop entirely removes a per-frame allocation source. The hotspot is in
the spark render loop (~line 2752).

Visual cost: no more transient particle bursts on bass/treble onsets.

### Throttle audio analysis

`AnalyserNode` runs in C++ but the per-frame `getByteFrequencyData` +
band averaging is JS. On a Pi this is small (single-digit ms) compared
to render but non-zero. Posting audio to the worker at half rate
(every other frame) would halve cross-thread message traffic. Visually
imperceptible because envelopes already smooth across frames.

### Disable strobe/invert flash

Flash is one full-screen fill or composite per trigger. Rare but heavy.
Set `flashTrigger` very high (e.g. `0.5`) so it effectively never fires.

---

## What NOT to disable

- **The WebGL dither pass itself** — it's already the cheap version
  (was 10–20ms on CPU, now milliseconds on GPU). Removing it changes
  the look entirely without buying back much budget.
- **The OffscreenCanvas worker path** — it doesn't add headroom but it
  keeps UI responsive. Stay on the worker path.
- **`preserveDrawingBuffer` on the dither canvas** — load-bearing for
  the mono-region passes that read from it later in the same frame.

---

## How `?lite` configures things

Adding `?lite` (or `?lite=1`) to the URL applies:

| Lever | Default | Lite |
|---|---|---|
| Max bitmap | `min(devicePixelRatio, 2)` × viewport | clamped to 1280×720 |
| `LATTICE_SPACING` | 24 | 36 |

Both can still be overridden live from the dev panel after load.

To go further on a Pi, the next levers worth pulling (in order) are:
`mesh3dCount` → 0, `grainAlpha` → 0.2, `FLYOUT_COUNT` → 5
(code change), `sliceBurstScale` → 0.5.
