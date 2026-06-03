#!/usr/bin/env python3
"""Combined VL53L1X / VL53L5CX bringup sanity check.

Auto-identifies which ToF sensor is on the bus — both default to I²C 0x29 but
report distinct model IDs, so the kiosk driver's own probe order picks the
right backend (L1X first, non-destructive; L5CX only if no L1X answers).

Override the auto-detect with the first CLI arg (or `--sensor=X`) or the
`VL53_SENSOR` env var: `auto` | `l1x` | `l5cx` (precedence: arg > env > config).

Run with the kiosk venv active, from the python/ dir so the package imports:

    cd python && source .venv/bin/activate
    python test_tof.py                 # auto-detect
    python test_tof.py l5cx            # force VL53L5CX
    VL53_SENSOR=l1x python test_tof.py # force VL53L1X via env

L1X: prints raw + rolling 1 s mean/stddev + ambient IR (tune
VL53_AMBIENT_LONG_MAX). L5CX: prints the closest valid zone + a live distance
grid so you can see the cone and aim it. Ctrl-C to exit.
"""

import collections
import logging
import os
import statistics
import sys
import time

# Run from python/ so the package is importable: `cd python && python test_tof.py`.
from ambient_kiosk import config
from ambient_kiosk.sensors import distance as D


def _resolve_choice(argv) -> str:
    """Sensor selection, precedence: CLI arg > VL53_SENSOR env > config default."""
    choice = None
    for a in argv[1:]:
        if a.startswith("--sensor="):
            choice = a.split("=", 1)[1]
            break
        if not a.startswith("-"):
            choice = a
            break
    if choice is None:
        choice = os.environ.get("VL53_SENSOR")
    if choice is None:
        choice = getattr(config, "VL53_SENSOR", "auto")
    choice = choice.lower()
    if choice not in ("auto", "l1x", "l5cx"):
        print(f"bad sensor choice {choice!r}; use auto | l1x | l5cx", file=sys.stderr)
        sys.exit(2)
    return choice


def _loop_l1x(backend) -> int:
    """Rolling raw / mean / stddev / ambient — mirrors the live auto-mode read."""
    sensor = backend._sensor
    print("\nhold target steady to read noise floor; wave to track motion")
    print("ambient = scene IR load (ST ULD units). Compare projector ON vs OFF,")
    print("on the real wall, to tune VL53_AMBIENT_LONG_MAX in config.py.")
    print("ctrl-c to exit\n")
    print(f"{'raw':>10}   {'mean(1s)':>10}   {'sd':>6}   {'amb':>6}   {'n':>3}")

    window = collections.deque(maxlen=50)  # ~1 s at 50 Hz
    last_print = 0.0
    last_d = None
    last_amb = None
    while True:
        if sensor.data_ready:
            d = sensor.distance  # cm, or None when no valid target in cone
            # Ambient must be read after the measurement and before clearing
            # the interrupt, same ordering the driver uses.
            last_amb = backend._read_ambient_rate()
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


def _loop_l5cx(backend) -> int:
    """Closest valid zone + a live distance grid (cm per zone, '--' = invalid)."""
    sensor = backend._sensor
    n = backend._grid_n
    side = 8 if n == 64 else 4
    print(f"\nclosest = nearest valid zone in the cone (what publishes as distance_cm)")
    print("grid shows per-zone distance in cm ('--' = no valid target this frame)")
    print("ctrl-c to exit\n")

    last_print = 0.0
    while True:
        if sensor.data_ready():
            data = sensor.get_data()
            dists = data.distance_mm
            stats = data.target_status
            m = min(n, len(dists), len(stats))
            cells = []
            valid_cm = []
            for z in range(m):
                if stats[z] in backend._VALID_STATUS and dists[z] > 0:
                    cm = dists[z] / 10.0
                    valid_cm.append(cm)
                    cells.append(f"{cm:5.0f}")
                else:
                    cells.append("   --")
            now = time.monotonic()
            if now - last_print >= 0.25:
                closest = min(valid_cm) if valid_cm else None
                closest_s = f"{closest:5.1f} cm" if closest is not None else "   -- "
                rows = "\n".join(
                    "  " + " ".join(cells[r * side:(r + 1) * side])
                    for r in range(side)
                )
                # Repaint in place: home cursor, clear below. Falls back to a
                # readable scroll if the terminal ignores the escape.
                sys.stdout.write("\033[H\033[J")
                print(f"[L5CX {side}x{side} @ {config.VL53L5CX_RANGING_HZ}Hz] "
                      f"closest={closest_s}  valid={len(valid_cm)}/{m}  "
                      f"far={backend.far_cm:.0f}cm\n")
                print(rows, flush=True)
                last_print = now
        time.sleep(0.01)


def main() -> int:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    choice = _resolve_choice(sys.argv)
    config.VL53_SENSOR = choice  # _make_backend honors this
    print(f"sensor selection: {choice}")

    try:
        i2c = D._open_i2c()
    except Exception as e:
        print(f"I²C open failed: {e}", file=sys.stderr)
        print("activate the kiosk venv and enable I²C (raspi-config)", file=sys.stderr)
        return 1

    if choice in ("auto", "l5cx"):
        print("probing"
              + (" (L5CX init uploads ~84 KB firmware, takes a moment)..."
                 if choice == "l5cx" else "..."))
    backend = D._make_backend(i2c)
    if backend is None:
        print(f"no supported ToF sensor detected at 0x29 (selection={choice}); "
              "check `i2cdetect -y 1` shows 0x29", file=sys.stderr)
        return 1
    print(f"detected: {backend.name}")
    if not backend.configure():
        print("sensor configure failed", file=sys.stderr)
        return 1
    print(f"far reach: {backend.far_cm:.0f} cm")

    try:
        if backend.name == "VL53L1X":
            return _loop_l1x(backend)
        return _loop_l5cx(backend)
    except KeyboardInterrupt:
        print()
        return 0
    finally:
        backend.stop()


if __name__ == "__main__":
    sys.exit(main())
