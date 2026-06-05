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

Both the onset (NEAR) and far reach (FAR) come **live from the sidecar**
— `distanceNearCm()` / `distanceFarCm()` read the `distance_near_cm` /
`distance_far_cm` topics — so they're tunable without editing JS (see
[Tuning the onset + reach](#tuning-the-onset--reach)). Fallback constants
at the top of the `applyAutomation` block in `static/index.html`:

- `DISTANCE_NEAR_DEFAULT_CM` = 75 (until `distance_near_cm` arrives)
- `DISTANCE_FAR_DEFAULT_CM` = 130 (until `distance_far_cm` arrives)

### `distance_cm` → `bitmapHeight` (opt-in)

When enabled, scales the authored bitmap-height ceiling down to a
64-pixel floor as someone walks away. The world coarsens with
distance.

| Distance | Effective `bitmapHeight` |
|---|---|
| ≤ NEAR | authored ceiling |
| ≥ FAR | 64 |
| NEAR-FAR | `1 / (1/ceiling + (1/64 − 1/ceiling)·x)`, `x = (d−NEAR)/(FAR−NEAR)` |

`NEAR` and `FAR` are the shared onset / mode-derived reach (NEAR default
75 cm; FAR ≈130 cm short / ≈400 cm long, from `distance_near_cm` /
`distance_far_cm`). The interpolation is harmonic — see below.

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

Onset (`distanceNearCm()`) and FAR (`distanceFarCm()`) are shared with
the twist mapping. The height is interpolated **harmonically** (linear in
`1/height`), not linearly: perceived pixelation scales with pixel size
∝ `1/height`, so a linear height ramp (or worse, a quadratic ease) looks
near-full-res for most of the range then collapses to MIN at the far end —
an abrupt switch, not a gradient. Bitmap-specific constants at the top of
the `applyAutomation` block in `static/index.html`:

- `DISTANCE_BITMAP_MIN` = 64 (pixel floor)
- `DISTANCE_BITMAP_QUANTIZE` = 12 (px step)

## Which sensor (VL53L1X or VL53L5CX)

`distance.py` supports two interchangeable ST ToF sensors behind a common
backend interface, chosen by `VL53_SENSOR` (`"auto"` | `"l1x"` | `"l5cx"`):

- **VL53L1X** — single-point. Short/long mode auto-select (below). The
  original sensor; behaviour unchanged.
- **VL53L5CX** — multizone (4×4 / 8×8). No short/long mode — a single
  ~4 m range (`VL53L5CX_FAR_CM`). Its zone grid is reduced to one number
  by taking the **closest valid zone in the cone** (`VL53L5CX_CONE_ZONES`
  restricts which zones count), so it publishes the *same* `distance_cm`
  and `distance_far_cm` topics — every downstream mapping is untouched.

Both default to I²C `0x29` but report distinct model IDs, so `"auto"`
probes the L1X first (a cheap, non-destructive model-ID read) and only
falls through to the L5CX (which uploads an ~84 KB firmware blob) when the
L1X isn't wired. Pin `VL53_SENSOR` explicitly for a deterministic install
boot. The L5CX wants VIN on the Pi's **5 V pin** (≈200 mA peak draw) with a
bulk decoupling cap at the breakout — see `hardware-handoff.md`.

## Distance mode & reach (VL53L1X)

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

## Tuning the onset + reach

The two endpoints of every distance→effect mapping — the **onset** (NEAR,
where distortion begins) and the **far reach** (FAR, where it saturates) —
are published by the Python sidecar and read by *both* the browser
(twist + bitmap) and the Daisy tape bridge. So there is **one knob each**,
no JS edits, and no Rust rebuild (the firmware only ever receives a
normalised 0..1 tape-failure over MIDI; all distance math is in JS):

| Endpoint | Topic | Source of truth | Tune by |
|---|---|---|---|
| onset | `distance_near_cm` | `config.py` `DISTANCE_NEAR_CM` (default 75) | edit config, or `DISTANCE_NEAR_CM=80 ./run_kiosk.sh` |
| far reach | `distance_far_cm` | `config.py` `VL53_FAR_CM_SHORT`/`_LONG` (sensor-mode derived) | edit config |

Both are re-published every ~2 s, so a Node/browser restart re-learns
them; the browser falls back to `DISTANCE_NEAR_DEFAULT_CM` / 
`DISTANCE_FAR_DEFAULT_CM` and the tape bridge to its own defaults until the
first value arrives. Install-day flow: change the value, restart the
sidecar, confirm via the `near=` / `far=` fields in the `?debug=1` overlay.

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
- After `NO_TARGET_TIMEOUT_S = 1.5` seconds (env-overridable) of
  continuous `None` reads, the smoothed value **snaps** to the
  mode-derived far reach (`_far_cm`; 130 cm short / 400 cm long). Snap
  (not gradual decay) is intentional — gradual decay leaves the
  visualizer thinking "user still close" for an extra ~150 ms after
  the hold expires. The default sits above the old 0.6 s to ride out
  dropouts (dark clothing, oblique torso, projector IR) that would
  otherwise flicker a present visitor to "empty"; the cost is a
  genuine walk-away taking ~1.5 s to register as idle.

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

### `python/test_tof.py`

Standalone bringup sanity check for either ToF sensor. Bypasses the
full kiosk software stack — reuses the driver's backend probe to
auto-identify L1X vs L5CX (override: first arg or `VL53_SENSOR` env),
then prints live readings. L1X shows raw + rolling 1 s mean/stddev +
ambient IR (vs a tape measure / for `VL53_AMBIENT_LONG_MAX` tuning);
L5CX shows the closest valid zone plus a live distance grid.

```sh
cd python && source .venv/bin/activate
python test_tof.py            # auto-detect
python test_tof.py l5cx       # force a sensor
```

If raw values from this script disagree with the SSE feed observed
in the browser, the divergence is in the sidecar/network path, not
the sensor.

### `?debug=1` overlay

Adds `&debug=1` to the visualizer URL to get a small top-right
overlay showing live mapping state. Works in lite mode (where the
dev panel is hidden):

```
toggle=on raw=119.0 d=120.0 ceil=1080 eff=72 bh=72 near=75 far=130 tg=0.67
```

| Field | Meaning |
|---|---|
| `toggle` | `manual.distanceToBitmap` — the bitmap-mapping enable state |
| `raw` | Latest SSE `distance_cm` (Python sidecar's published value) |
| `d` | Browser-side smoothed distance (`_smoothedDistance`) |
| `ceil` | Authored bitmap ceiling (slider/lane value) |
| `eff` | Computed effective bitmap (post quantization) |
| `bh` | Actual `bitmapHeight` runtime variable in computeDpr() |
| `near` | Active onset (`distance_near_cm`; default 75) |
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
| `DISTANCE_NEAR_CM` | config.py (env-overridable) | 75 | **Onset** — no distortion out to here. Single source of truth, published as `distance_near_cm` to browser + tape bridge |
| `DISTANCE_NEAR_DEFAULT_CM` | static/index.html | 75 | Browser onset fallback until `distance_near_cm` arrives |
| `DISTANCE_FAR_DEFAULT_CM` | static/index.html | 130 | FAR fallback until `distance_far_cm` arrives |
| `DISTANCE_BITMAP_MIN` | static/index.html | 64 | Lower = more aggressive low-res floor when far |
| `DISTANCE_BITMAP_QUANTIZE` | static/index.html | 12 | Fewer resize events, but visibly coarser steps |
| `DISTANCE_SMOOTH_TAU_S` | static/index.html | 0.25 | Slower response, less noise |
| `NO_TARGET_TIMEOUT_S` | config.py (env) | 1.5 | Rides out longer dropouts at cost of slower "walked away" |
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
