# Performance degrade options for low-power devices

Reference target: Raspberry Pi 4 Model B (VideoCore VI, single-channel
LPDDR4) running Chromium at 1080p. Same advice applies to other weak
GPUs, low-end ARM Chromebooks, etc.

**Revised diagnosis (after measurement, supersedes the earlier note):**
On a Pi 4 the actual bottleneck is **GPU memory bandwidth on the per-frame
WebGL texture uploads**, not Canvas2D fillrate or stamp count. The
dither and twist passes each do a `texImage2D(canvas)` per frame, which
copies the entire main canvas into a GL texture (~3.7 MB at 720p × 2
passes × 60 Hz = ~440 MB/s sustained). The V3D memory bus saturates
before the dither shader even runs, and per-frame work spills across
multiple vsync periods — observable as fps locked at 8–9 even though
the JS work is only 12–15 ms per frame.

Canvas2D CPU profiling (lattice, grain regen, etc.) significantly
*undercounted* this because `gl.drawArrays()` returns instantly; the
GPU bandwidth cost is invisible to `performance.now()` instrumentation.

**The big lever** is therefore: shrink the bitmap that gets uploaded.
`?bitmap=N` caps render height to N pixels (browser upscales for
display). 360p ≈ 4× less GPU bandwidth than 720p and gets to acceptable
fps; the dither aesthetic actually *benefits* from chunkier upscaling.

Items are grouped by approximate impact. Numbers in parens are the
default values in `static/index.html`. Many of these are already exposed as
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

`static/index.html:2282` and `static/index.html:2285`:
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

`static/index.html:629`:
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

`static/index.html:1289`:
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

`static/index.html:528`:
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

`static/index.html:540` (`const`) and `static/index.html:619` (`let`, live param).

### Increase scanline period

`SCANLINE_PERIOD` (default `2`) — every Nth row gets darkened. The
implementation now uses a single tiled fillRect (optimization #5 in
`OPTIMIZATIONS.md`) so this is cheap, but going to `3` or `4`
incrementally reduces overdraw on the tile.

`static/index.html:541` — currently `const`. Change requires rebuilding
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

## URL flags for low-power devices

| Flag | Effect |
|---|---|
| `?lite=1` | Hides all DOM overlays (UI, timeline, editor, drop hints) so the compositor merges only the canvas. Caps bitmap to 1280×720. Bumps `LATTICE_SPACING` 24 → 36 |
| `?bitmap=N` | Caps render bitmap height to N px. Width scales with viewport aspect. Browser upscales to fill display. **Highest-leverage knob on Pi 4** — GPU bandwidth scales with bitmap². Also exposed as the `bitmapHeight` PARAMS slider so it can be tuned at runtime; the URL sets the initial ceiling |
| `?distanceToBitmap=on` | Wires the VL53L1X reading to scale `bitmapHeight` from the authored ceiling down to a 64 px floor as someone approaches. Off by default; URL param wins over localStorage. See `SENSOR_MAPPING.md` |
| `?debug=1` | Shows a small top-right diagnostic overlay (toggle / raw distance / smoothed distance / effective bitmap / twistGain). Works in lite mode. See `SENSOR_MAPPING.md` |
| `?profile=1` | Enables per-section worker render timing. Logs to console once/sec and shows in dev panel. Cost: ~14 `performance.now()` calls/frame |
| `?nogl=1` | Skips dither + twist WebGL passes. Diagnostic for engines that can't get a WebGL context on `OffscreenCanvas` (e.g. cog/WPEWebKit). Visual cost: no dither, no twist |
| `?nooffscreen=1` | Forces main-thread renderer (worker path is the default). For engines where `transferControlToOffscreen` fails silently |

**Pi 4 kiosk recommended:** `?lite=1&bitmap=360&distanceToBitmap=on`.
The `bitmap=360` baseline gets to acceptable fps by reducing WebGL
texture upload bandwidth ~4×. Chunky dither aesthetic is on-brand.
`distanceToBitmap=on` drops the bitmap further when someone is close
to the sensor — perf cost during approach is offset by the smaller
bitmap making the GPU faster, so the visual response feels snappy
even though resolution drops.

To go further: lower `bitmap` (`240`?), disable mesh3d (slider:
`mesh3dCount → 0`), reduce `FLYOUT_COUNT` (constant in
`static/index.html`, requires reload).

---

## Kiosk pipeline overhead

When running with the Node SSE bridge (`server/`) and Python sensor
sidecar (`python/`), the additional steady-state cost on a Pi 4 is:

| Process | CPU (1 core) | Memory |
|---|---|---|
| Node bridge | < 0.1% | ~50 MB |
| Python sidecar | 1–3% | ~60 MB |
| `pigpiod` daemon | 1–3% | small |

Together: ~5% of one core, ~110 MB. The Pi 4's 4 cores easily absorb
this — Chromium continues to spread its renderer/compositor/raster/GPU
processes across 2–3 cores, with the 4th left over for the sensor stack
and OS. Sensor I/O is 50 Hz of I²C reads and a handful of edges; against
~2 million Canvas2D pixel ops per frame it's rounding error.

**Footgun to avoid in the breath driver** — `BreathDriver` registers
the pigpio rising-edge callback once and samples a running counter
every 200 ms. Do not rewrite it to the per-window setup/teardown
pattern shown in `hardware-handoff.md`'s example: at the worst-case
~10 kHz edge rate, callback setup/teardown churn becomes a meaningful
fraction of one core. The current implementation comment in
`python/ambient_kiosk/sensors/breath.py` flags this; preserve it.

---

## Going native (wgpu) vs the in-browser FBO migration

(2026-06-06 analysis — "would a native wgpu app have fewer compositor ops /
better FPS than Chromium?")

**Compositor operations are not the bottleneck.** The diagnosis above stands:
the wall is per-frame `texImage2D(canvas)` GPU-bandwidth, the bridge from the
**Canvas2D** main renderer into the WebGL dither/lattice post passes. Compositing
one fullscreen layer is cheap next to that, so dropping Chromium's compositor (or
going direct DRM/KMS scanout) saves a small term, not the dominant one.

**A native wgpu app *could* raise the FPS ceiling — but via different mechanisms
than "fewer compositor ops":**

1. **No CPU-canvas → GPU bridge.** A native GPU renderer keeps everything
   GPU-resident, so the `texImage2D(canvas)` upload — the dominant cost —
   disappears by construction (no Canvas2D surface to upload).
2. **No Chromium GPU-process / command-buffer tax.** WebGL calls are serialized
   through the GPU process; native talks to Mesa/V3D directly. (This is part of
   why CPU profiling *undercounts* the real cost.)
3. **No WebGL upload conversions** (flipY / premultiply / colorspace on
   `texImage2D`).

**The catch:** the renderer is **Canvas2D-native** (the entire visual language is
`drawImage`/paths/fills). Porting to wgpu/WGSL is a from-scratch reimplementation
of every visual element as GPU geometry + shaders, *plus* re-doing the FFT
(currently Web Audio `AnalyserNode`), the SSE sensor bridge, the bpm/keypoint
lanes, and the `localaudio` POS-sync — all browser-integrated. Months-scale.
wgpu on Pi 4 is viable (v3dv Vulkan / v3d GLES); the renderer port is the risk,
not the backend.

**Higher-ROI path (already underway):** finish migrating the remaining Canvas2D
compositing to **FBO/WebGL-resident** rendering so the per-frame
`texImage2D(canvas)` upload is eliminated *inside the browser* (see the
"Replaces the per-frame texImage2D" / FBO / atlas comments in
`static/index.html`). That captures mechanism #1 — the dominant term — without
leaving Chromium or rewriting the interaction/audio/sensor stack.

**Measure before either:**
- Sweep `?bitmap=N` — if FPS scales strongly with bitmap size, you're
  upload-bandwidth-bound (FBO migration / native is the answer; compositor ops
  irrelevant).
- Check whether the fullscreen Chromium surface already gets **direct scanout**
  (compositor/`drm_info` overlay stats). If it does, the compositor-ops savings
  from going native are ~zero.
