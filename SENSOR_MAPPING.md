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
effect that builds as someone approaches the kiosk.

| Distance | `twistGain` | Effective `maxTwistDeg` |
|---|---|---|
| ≤ 25 cm | 1.0 | authored ceiling (full) |
| ≥ 100 cm | 0 | 0 (no twist) |
| 25-100 cm | `1 - x²` where `x = (d-25)/75` | gain × authored |

Curve is *ease-in toward zero* — the effect holds near-max for most of
the close range, then drops off rapidly as someone walks away. The
dev-panel slider keeps showing the authored ceiling; only the value
posted to the worker is scaled.

Constants at top of the `applyAutomation` block in
`static/index.html`:

- `DISTANCE_TWIST_NEAR_CM` = 25
- `DISTANCE_TWIST_FAR_CM` = 100

### `distance_cm` → `bitmapHeight` (opt-in)

When enabled, scales the authored bitmap-height ceiling down to a
64-pixel floor as someone approaches. The world coarsens with
proximity.

| Distance | Effective `bitmapHeight` |
|---|---|
| ≥ 100 cm | authored ceiling |
| ≤ 25 cm | 64 |
| 25-100 cm | `64 + (ceiling - 64) · x²` where `x = (d-25)/75` |

Curve is *ease-out from MIN* — sticky at the low-res "dissociation"
state when close, then recovers rapidly as someone leaves. Quantized
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

Constants at top of the `applyAutomation` block in
`static/index.html`:

- `DISTANCE_BITMAP_NEAR_CM` = 25
- `DISTANCE_BITMAP_FAR_CM` = 100
- `DISTANCE_BITMAP_MIN` = 64 (pixel floor)
- `DISTANCE_BITMAP_QUANTIZE` = 20 (px step)

## Smoothing

Two layers, designed to filter noise without lagging the "person
arrived/left" transitions.

### Python sidecar (`python/ambient_kiosk/sensors/distance.py`)

- Valid sensor reads are smoothed with EMA, `α = VL53_SMOOTH_ALPHA =
  0.25` at 50 Hz publish rate (~80 ms tau).
- `None` reads (sensor dropout — common on shiny targets, motion
  edges, below-min-range, etc.) **hold** the smoothed value rather
  than decaying. Without this, every dropped frame would yank the
  published value toward `VL53_FAR_CM`, biasing the SSE feed upward
  even with a target consistently present.
- After `NO_TARGET_TIMEOUT_S = 0.6` seconds of continuous `None`
  reads, the smoothed value **snaps** to `VL53_FAR_CM = 100`. Snap
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

Net response time, "person walks out of cone" → "bitmap at full":
~0.9 s (0.6 s hold + 0.25 s browser EMA convergence).

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
toggle=on raw=47.3 d=47.5 ceil=1080 eff=240 bh=240 tg=0.70
```

| Field | Meaning |
|---|---|
| `toggle` | `manual.distanceToBitmap` — the bitmap-mapping enable state |
| `raw` | Latest SSE `distance_cm` (Python sidecar's published value) |
| `d` | Browser-side smoothed distance (`_smoothedDistance`) |
| `ceil` | Authored bitmap ceiling (slider/lane value) |
| `eff` | Computed effective bitmap (post quantization) |
| `bh` | Actual `bitmapHeight` runtime variable in computeDpr() |
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
| `DISTANCE_TWIST_NEAR_CM` | static/index.html | 25 | Twist holds max over a wider close zone |
| `DISTANCE_TWIST_FAR_CM` | static/index.html | 100 | Twist still partially active at larger distances |
| `DISTANCE_BITMAP_NEAR_CM` | static/index.html | 25 | Bitmap floor (MIN) starts further out |
| `DISTANCE_BITMAP_FAR_CM` | static/index.html | 100 | Bitmap reaches ceiling later |
| `DISTANCE_BITMAP_MIN` | static/index.html | 64 | Less aggressive low-res floor when close |
| `DISTANCE_BITMAP_QUANTIZE` | static/index.html | 20 | Fewer resize events, but visibly coarser steps |
| `DISTANCE_SMOOTH_TAU_S` | static/index.html | 0.25 | Slower response, less noise |
| `NO_TARGET_TIMEOUT_S` | distance.py | 0.6 | Rides out longer dropouts at cost of slower "walked away" |
| `VL53_SMOOTH_ALPHA` | config.py | 0.25 | Higher = more responsive (more noise) |
| `VL53_FAR_CM` | config.py | 100 | Snap target — also determines the FAR end of the mappings |

## See also

- `hardware-handoff.md` — VL53L1X mounting, distance modes, expected
  noise floor (§VL53L1X).
- `PI_KIOSK_BRINGUP.md` — Phase 4 (distance sensor alone), kiosk
  autostart URL.
- `PI_PERFORMANCE.md` — `?bitmap=N` URL param origin and the
  GPU-bandwidth case for low render bitmaps.
- `python/README.md` — sidecar architecture and per-sensor
  publication semantics.
