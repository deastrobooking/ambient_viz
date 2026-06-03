# Sensor → visualizer mapping

How sensor inputs (VL53L1X, MPR121, etc.) drive visualizer parameters
in real time. This doc complements `hardware-handoff.md` (the
hardware spec) and `PI_KIOSK_BRINGUP.md` (the bringup runbook) — it
covers the *interaction layer* that sits between the two.

## Pipeline overview

```
[Pi GPIO/I²C] ──► [ambient_kiosk] ── POST /ingest ──► [server/] ── SSE /events ──► [Browser]
                       (Python)                       (Node bridge)                  │
                                                                                     ▼
                                                                            window.AMBIENT_INPUTS
                                                                                     │
                                                                                     ▼
                                                                            applyAutomation()
                                                                            per-frame param mapping
                                                                                     │
                                                                                     ▼
                                                                            worker postMessage
                                                                            or setLive() / resize()
```

`window.AMBIENT_INPUTS` is the live snapshot of every sensor reading,
keyed by the wire name (`distance_cm`, `motion`, `touch_mask`,
`breath_detected`). The visualizer reads it opportunistically inside
`applyAutomation()` each rAF tick.

## Current mappings

### `distance_cm` → `maxTwistDeg` (always on)

Scales the authored `maxTwistDeg` slider value by a distance-derived
gain. Implements the [[artistic-statement-pain-material]] idea of an
effect that builds as someone *retreats* from the kiosk — close
inspection leaves the image untouched, distance pulls it apart.

| Distance | `twistGain` | Effective `maxTwistDeg` |
|---|---|---|
| ≤ 75 cm | 0 | 0 (no twist) |
| ≥ FAR | 1.0 | authored ceiling (full) |
| 75-FAR cm | `x²` where `x = (d-75)/(FAR-75)` | gain × authored |

Curve is *ease-in from zero* — no distance-induced twist within 75 cm,
then a subtle onset that accelerates to full as someone walks away. The
dev-panel slider keeps showing the authored ceiling; only the value
posted to the worker is scaled.

`FAR` is **not fixed**: it tracks the sensor's actual reach, which
depends on the distance mode the Python sidecar auto-selects at boot
(short ≈ 130 cm, long ≈ 400 cm). The sidecar publishes that reach as the
`distance_far_cm` SSE topic; the browser reads it live (`distanceFarCm()`)
and falls back to 130 cm until the first value arrives. See
[Distance mode & reach](#distance-mode--reach) below.

Constants at top of the `applyAutomation` block in
`static/index.html`:

- `DISTANCE_NEAR_CM` = 75 (shared onset for twist + bitmap)
- `DISTANCE_FAR_DEFAULT_CM` = 130 (fallback until `distance_far_cm` arrives)

### `distance_cm` → `bitmapHeight` (opt-in)

When enabled, scales the authored bitmap-height ceiling down to a
64-pixel floor as someone walks away. The world coarsens with
distance.

| Distance | Effective `bitmapHeight` |
|---|---|
| ≤ 75 cm | authored ceiling |
| ≥ FAR | 64 |
| 75-FAR cm | `ceiling - (ceiling - 64) · x²` where `x = (d-75)/(FAR-75)` |

`FAR` is the same mode-derived reach as the twist mapping (≈130 cm short
/ ≈400 cm long, from `distance_far_cm`).

Curve is *ease-in toward MIN* — full res within 75 cm, then the low-res
"dissociation" state builds as someone leaves, hitting the floor at the
FAR reach. Quantized
to 20-pixel steps to bound the GPU buffer realloc rate. Routes
through `PARAMS.bitmapHeight.setLive()` (which mutates the
main-thread variable and calls `resize()`); the worker's `params`
dispatch ignores `bitmapHeight`, so this is the only path that
actually changes the rendered resolution.

Enable mechanisms (in precedence order):

1. **URL param: `?distanceToBitmap=on`** — wins over localStorage.
   Use this in the kiosk autostart URL since lite mode hides the dev
   panel.
2. **Dev-panel dropdown** — `distanceToBitmap` discrete control,
   visible in non-lite mode.
3. **Default**: `off`. The visualizer behaves exactly as before
   without the mapping.

Onset (`DISTANCE_NEAR_CM` = 75) and FAR (`distanceFarCm()`) are shared
with the twist mapping. Bitmap-specific constants at the top of the
`applyAutomation` block in `static/index.html`:

- `DISTANCE_BITMAP_MIN` = 64 (pixel floor)
- `DISTANCE_BITMAP_QUANTIZE` = 20 (px step)

## Distance mode & reach

The VL53L1X runs in one of two distance modes, auto-selected at boot by
the Python sidecar from an ambient-IR sample (`VL53_AUTO_MODE`,
`_calibrate_distance_mode` in `distance.py`):

| Mode | Reach (`distance_far_cm`) | Timing budget | Ranging rate |
|---|---|---|---|
| short | `VL53_FAR_CM_SHORT` = 130 cm | 20 ms | ~50 Hz |
| long | `VL53_FAR_CM_LONG` = 400 cm | 200 ms | ~5 Hz |

- **Why the budget changes with mode.** Per the datasheet, the 20 ms
  budget is valid *only* in short mode; long mode needs ≥ 33 ms to range
  at all and ≥ 140 ms to reach the full 4 m. The Adafruit lib only
  accepts a discrete set `{15,20,33,50,100,200,500}`, so long mode uses
  **200 ms** (the next step ≥ 140). That drops the ranging rate to ~5 Hz,
  which the `None`-hold logic and browser EMA absorb. (`_budget_for_mode`.)
- **Reach is published, not assumed.** Because the active mode isn't
  known until boot, the chosen reach is published as the `distance_far_cm`
  topic and re-sent every ~2 s (so a Node/browser restart re-learns it).
  All three effect consumers read it as the FAR end of their ramp:
  - browser twist + bitmap — `distanceFarCm()` in `static/index.html`
  - tape failure — `farCm` in `server/src/inputs/daisy-position.js`
- **Real-world reach.** 4 m is the best-case long-mode figure (dark room,
  white target). A person's clothing is low-reflectivity, so they often
  read `None` somewhere around 2.5–3.6 m → which snaps to the far value →
  max effect anyway. So the smooth ramp covers the trackable range and
  "very far / absent" lands on full destruction regardless.

## Smoothing

Two layers, designed to filter noise without lagging the "person
arrived/left" transitions.

### Python sidecar (`python/ambient_kiosk/sensors/distance.py`)

- Valid sensor reads are smoothed with EMA, `α = VL53_SMOOTH_ALPHA =
  0.25` at 50 Hz publish rate (~80 ms tau).
- `None` reads (sensor dropout — common on shiny targets, motion
  edges, below-min-range, etc.) **hold** the smoothed value rather
  than decaying. Without this, every dropped frame would yank the
  published value toward the far reach, biasing the SSE feed upward
  even with a target consistently present.
- After `NO_TARGET_TIMEOUT_S = 0.6` seconds of continuous `None`
  reads, the smoothed value **snaps** to the mode-derived far reach
  (`_far_cm`; 130 cm short / 400 cm long). Snap
  (not gradual decay) is intentional — gradual decay leaves the
  visualizer thinking "user still close" for an extra ~150 ms after
  the hold expires.

The history of these knobs is in the commit log; the short version
is that the original 2 s hold + EMA decay produced ~3 s of "kiosk
still thinks you're here" lag after departure.

### Browser (`static/index.html`)

- Lightweight dt-based EMA, `DISTANCE_SMOOTH_TAU_S = 0.25` (~80%
  convergence in 0.25 s).
- dt-based formulation means tau stays correct if frame rate varies
  or applyAutomation skips a tick.
- Mostly there to soften the snap-to-FAR moment so it doesn't read
  as a hard cut. The Python side already filters most of the
  jitter.

Net response time, "person walks out of cone" → "bitmap at MIN /
twist at full": ~0.9 s (0.6 s hold + 0.25 s browser EMA convergence).

## Operational tooling

### `./run_kiosk.sh`

Single-command launcher for the Node bridge + Python sidecar.
Interleaves their stdout with `[node]` / `[py  ]` prefixes; Ctrl-C
kills both cleanly. Echoes the recommended browser URL (including
`?distanceToBitmap=on`) at startup.

Default sidecar args: `--no-pir --no-breath` (distance + touch
enabled). Pass overrides as positional args, e.g.:

```sh
./run_kiosk.sh --mock                # synthetic data, no hardware
./run_kiosk.sh --no-touch            # distance only
```

### `python/test_vl53l1x.py`

Standalone bringup sanity check. Bypasses the full kiosk software
stack — talks straight to the sensor library and prints live
readings plus rolling 1 s mean/stddev so you can validate accuracy
(vs a tape measure) and noise floor (target held steady).

```sh
cd python && source .venv/bin/activate
python test_vl53l1x.py
```

If raw values from this script disagree with the SSE feed observed
in the browser, the divergence is in the sidecar/network path, not
the sensor.

### `?debug=1` overlay

Adds `&debug=1` to the visualizer URL to get a small top-right
overlay showing live mapping state. Works in lite mode (where the
dev panel is hidden):

```
toggle=on raw=119.0 d=120.0 ceil=1080 eff=400 bh=400 far=130 tg=0.67
```

| Field | Meaning |
|---|---|
| `toggle` | `manual.distanceToBitmap` — the bitmap-mapping enable state |
| `raw` | Latest SSE `distance_cm` (Python sidecar's published value) |
| `d` | Browser-side smoothed distance (`_smoothedDistance`) |
| `ceil` | Authored bitmap ceiling (slider/lane value) |
| `eff` | Computed effective bitmap (post quantization) |
| `bh` | Actual `bitmapHeight` runtime variable in computeDpr() |
| `far` | Active FAR reach (`distance_far_cm`; 130 short / 400 long mode) |
| `tg` | `twistGain` factor multiplying `maxTwistDeg` |

A divergence between `raw` and `d` indicates browser smoothing in
action. A divergence between `eff` and `bh` would indicate that
`setLive()` isn't being called — investigate the bitmap-scaling
block in `applyAutomation`.

`window.__distbm` exposes the same state object for console
inspection, regardless of `?debug=1`.

## Tuning summary

| Knob | Where | Default | Effect of increase |
|---|---|---|---|
| `DISTANCE_NEAR_CM` | static/index.html | 75 | Onset distance — no distortion out to here (shared twist + bitmap) |
| `DISTANCE_FAR_DEFAULT_CM` | static/index.html | 130 | FAR fallback until `distance_far_cm` arrives |
| `DISTANCE_BITMAP_MIN` | static/index.html | 64 | Lower = more aggressive low-res floor when far |
| `DISTANCE_BITMAP_QUANTIZE` | static/index.html | 20 | Fewer resize events, but visibly coarser steps |
| `DISTANCE_SMOOTH_TAU_S` | static/index.html | 0.25 | Slower response, less noise |
| `NO_TARGET_TIMEOUT_S` | distance.py | 0.6 | Rides out longer dropouts at cost of slower "walked away" |
| `VL53_SMOOTH_ALPHA` | config.py | 0.25 | Higher = more responsive (more noise) |
| `VL53_FAR_CM_SHORT` | config.py | 130 | Short-mode reach + FAR end of the mappings (snap target) |
| `VL53_FAR_CM_LONG` | config.py | 400 | Long-mode reach + FAR end of the mappings (snap target) |
| `VL53_TIMING_BUDGET_MS_LONG` | config.py | 200 | Larger = longer reach + better repeatability, slower ranging |
| `VL53_AMBIENT_LONG_MAX` | config.py | 1500 | Higher = picks long mode in brighter scenes (less reliable) |

## See also

- `hardware-handoff.md` — VL53L1X mounting, distance modes, expected
  noise floor (§VL53L1X).
- `PI_KIOSK_BRINGUP.md` — Phase 4 (distance sensor alone), kiosk
  autostart URL.
- `PI_PERFORMANCE.md` — `?bitmap=N` URL param origin and the
  GPU-bandwidth case for low render bitmaps.
- `python/README.md` — sidecar architecture and per-sensor
  publication semantics.
