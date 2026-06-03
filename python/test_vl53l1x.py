#!/usr/bin/env python3
"""Standalone VL53L1X bringup sanity check.

Run with the kiosk venv active:

    cd python && source .venv/bin/activate
    python test_vl53l1x.py

Prints live readings plus rolling mean/stddev over the last ~1 s window,
so you can validate accuracy (vs a tape measure) and noise floor (target
held steady) before plugging into the full kiosk pipeline. Ctrl-C to exit.
"""

import collections
import logging
import statistics
import sys
import time

try:
    import board
    import busio
    import adafruit_vl53l1x
except ImportError as e:
    print(f"missing dep: {e}", file=sys.stderr)
    print("activate the kiosk venv first: cd python && source .venv/bin/activate", file=sys.stderr)
    sys.exit(1)

# Reuse the live kiosk driver's ambient auto-select. Run from python/ so the
# package is importable: `cd python && python test_vl53l1x.py`.
from ambient_kiosk import config
from ambient_kiosk.sensors.distance import DistanceDriver


def main() -> int:
    i2c = busio.I2C(board.SCL, board.SDA)
    try:
        sensor = adafruit_vl53l1x.VL53L1X(i2c)
    except Exception as e:
        print(f"VL53L1X not found at 0x29: {e}", file=sys.stderr)
        print("check `i2cdetect -y 1` first — 0x29 must be visible", file=sys.stderr)
        return 1

    # Auto-select short vs long mode from ambient IR by reusing the kiosk
    # driver's own calibration, so the test lands on the same mode the live
    # pipeline would pick at boot. Start in short mode for the ambient sample
    # (mirrors DistanceDriver._init_sensor), range, then calibrate.
    sensor.distance_mode = 1   # short: ambient-safe starting point
    sensor.timing_budget = config.VL53_TIMING_BUDGET_MS
    sensor.start_ranging()

    # Surface the driver's calibration logging (ambient median + chosen mode).
    logging.basicConfig(level=logging.INFO, format="%(message)s")

    # Bringup tool: deliberately drive the driver's internals with our own
    # sensor handle rather than spinning up its thread/ingest.
    driver = DistanceDriver(ingest=None)
    driver._sensor = sensor
    print(f"sampling ambient IR for {config.VL53_AMBIENT_CAL_S:g}s to auto-select mode...")
    mode = driver._calibrate_distance_mode(1)
    mode_name = "long" if mode == 2 else "short"

    # _calibrate_distance_mode applies the chosen mode's budget on a switch, so
    # report that (not the short-mode sample budget) and the resulting far reach.
    print(f"VL53L1X ready: {mode_name} mode, "
          f"{driver._budget_for_mode(mode)} ms timing budget, "
          f"far reach {driver._far_for_mode(mode):.0f} cm")
    print("hold target steady to read noise floor; wave to track motion")
    print("ambient = scene IR load (ST ULD units). Compare projector ON vs OFF,")
    print("on the real wall, to tune VL53_AMBIENT_LONG_MAX in config.py.")
    print("ctrl-c to exit\n")
    print(f"{'raw':>10}   {'mean(1s)':>10}   {'sd':>6}   {'amb':>6}   {'n':>3}")

    # Reuse the driver's ambient read so the printed number is exactly what
    # auto-mode-select used above — no duplicated register logic to drift.
    read_ambient = driver._read_ambient_rate

    window = collections.deque(maxlen=50)  # ~1 s at 50 Hz
    last_print = 0.0
    last_d = None
    last_amb = None
    try:
        while True:
            if sensor.data_ready:
                d = sensor.distance  # cm, or None when no valid target in cone
                last_amb = read_ambient()
                sensor.clear_interrupt()
                last_d = d
                if d is not None:
                    window.append(d)
            now = time.monotonic()
            if now - last_print >= 0.1:
                raw_s = f"{last_d:6.1f} cm" if last_d is not None else "    --   "
                amb_s = f"{last_amb:>6d}" if last_amb is not None else f"{'--':>6}"
                if len(window) >= 2:
                    mu = statistics.mean(window)
                    sd = statistics.stdev(window)
                    print(f"{raw_s:>10}   {mu:6.1f} cm   {sd:5.2f}   {amb_s}   {len(window):>3}",
                          end="\r", flush=True)
                else:
                    print(f"{raw_s:>10}   {'--':>10}   {'--':>6}   {amb_s}   {len(window):>3}",
                          end="\r", flush=True)
                last_print = now
            time.sleep(0.005)
    except KeyboardInterrupt:
        print()
        if len(window) >= 2:
            print(f"final: n={len(window)} "
                  f"mean={statistics.mean(window):.1f} cm "
                  f"sd={statistics.stdev(window):.2f} cm")
        sensor.stop_ranging()
    return 0


if __name__ == "__main__":
    sys.exit(main())
