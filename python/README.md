# ambient_kiosk — Python sensor sidecar

Reads the four kiosk sensors per `../hardware-handoff.md` and POSTs each
value change to the Node SSE bridge at `http://127.0.0.1:8080/ingest`.
The Node server fans events out to the browser.

```
[Pi GPIO/I²C] ──► [ambient_kiosk] ── POST /ingest ──► [server/] ── /events ──► [Browser]
```

## Install (on the Pi)

```sh
cd python
# Enable the pigpio daemon (needed by the breath driver)
sudo systemctl enable --now pigpiod

# Editable install + Pi-only deps
pip install -e .
```

`pyproject.toml` marks the hardware libraries (gpiozero, pigpio,
adafruit-blinka, adafruit-circuitpython-vl53l1x, adafruit-circuitpython-mpr121)
as Pi-only via PEP 508 platform markers. On non-Pi machines they're
skipped, and `--mock` works without them.

## Run

```sh
# Pi, hardware
python -m ambient_kiosk

# Mac or Pi, synthetic data (no hardware needed)
python -m ambient_kiosk --mock

# With auth (must match server's INGEST_TOKEN)
INGEST_TOKEN=devsecret python -m ambient_kiosk

# Skip a sensor (e.g. if it's not wired yet)
python -m ambient_kiosk --no-breath
```

The Node server must be running first (`cd ../server && npm start`).

## Sensors

| Module | Hardware | Pin / Bus | Publishes |
|---|---|---|---|
| `sensors.pir` | AM312 PIR | GPIO4 | `motion` (bool) |
| `sensors.distance` | VL53L1X ToF | I²C 0x29 | `distance_cm` (number, 0.1 cm step) |
| `sensors.breath` | HR202 + TLC555 | GPIO17 (edge count) | `breath_detected` (ms timestamp) |
| `sensors.touch` | MPR121 | I²C 0x5A + IRQ GPIO27 | `touch_mask` (12-bit int) |

All values land on the SSE bus and become available in the browser as
`window.AMBIENT_INPUTS.<name>`.

## Notes

- **AM312 startup suppression**: `PirDriver` ignores the sensor for the
  first 60 s after process start. This is mandatory per the handoff doc
  — the AM312's output is unreliable during settling.
- **TLC555 callback pattern**: `BreathDriver` registers the pigpio
  rising-edge callback **once** at startup and samples a running
  counter every 200 ms. Do not rewrite this to the per-window
  setup/teardown shown in the handoff doc's example — at the worst-case
  ~10 kHz edge rate that becomes a meaningful CPU hit. The implementation
  comment explains this.
- **VL53L1X invalid reads**: `None` from the sensor is treated as a
  dropout, not as "target at FAR." The smoothed value is **held**
  during brief dropouts (which happen constantly on real targets) and
  **snaps** to `VL53_FAR_CM` after `NO_TARGET_TIMEOUT_S = 0.6` seconds
  of continuous silence. This avoids the upward bias the original
  decay-toward-FAR design introduced into the published `distance_cm`
  feed. Idle visualizer state is "nobody here," and is reached within
  ~0.6 s of departure (not 2-3 s as in earlier versions).
- **ToF bringup**: `python test_tof.py` is a standalone sanity-check
  script (talks straight to the sensor library, no sidecar / Node /
  browser). Auto-detects L1X vs L5CX (override with the first arg or
  `VL53_SENSOR` env); L1X prints raw + rolling 1 s mean/stddev, L5CX
  prints the closest zone + a live grid. Use it during Phase 4 of
  `PI_KIOSK_BRINGUP.md` to validate the sensor before the SSE pipeline.
- **MPR121 baseline auto-cal**: first ~30 s after init, touch behavior
  may be inconsistent while the MPR121 calibrates to its environment.
  The handoff doc flags this.

## Tuning

`config.py` is the single source of truth for pins, thresholds, and
rates. Match it to `../hardware-handoff.md` if you change either.
