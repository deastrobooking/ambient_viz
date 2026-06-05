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

L1X: live raw + rolling 1 s mean/stddev + ambient IR (tune VL53_AMBIENT_LONG_MAX).
L5CX: closest valid zone + a live distance grid so you can see the cone and aim it.

Both views also run the real empty-room learner and show it live — smoothed
distance, velocity, the stillness-window fill, and the learned "full-destruction"
far reach — so you can tune EMPTY_ROOM_* in config.py (or via env, e.g.
`EMPTY_ROOM_VELOCITY_CM_S=0.5 EMPTY_ROOM_STILLNESS_WINDOW_S=8 python test_tof.py`)
and watch the empty room get learned. Ctrl-C to exit.
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


class _NullIngest:
    """Swallows publishes — the tuning view reads driver state directly."""
    def publish(self, name, value):  # noqa: D401  (stub)
        pass


def _make_driver(backend):
    """A DistanceDriver wired to an already-configured backend so the tuning
    view runs the EXACT live smoothing + velocity + empty-room learner (no
    logic duplicated here). _process_sample is fed the per-frame read; the
    learner mutates the driver's far reach just as it does in the kiosk."""
    drv = D.DistanceDriver(_NullIngest(), mock=False)
    drv._backend = backend
    drv._far_cm = backend.far_cm
    drv._far_ceiling = backend.far_cm
    return drv


def _bar(frac, width=24):
    frac = 0.0 if frac < 0 else (1.0 if frac > 1 else frac)
    fill = int(round(frac * width))
    return "#" * fill + "-" * (width - fill)


def _empty_room_lines(drv, now):
    """Multi-line live readout of the empty-room learner for tuning."""
    s = drv.empty_room_status(now)
    sm = f"{s['smoothed']:.1f} cm" if s['smoothed'] is not None else "--"
    vel = f"{s['velocity']:+.2f} cm/s" if s['velocity'] is not None else "--"
    thr = config.EMPTY_ROOM_VELOCITY_CM_S
    er = f"{s['empty_room']:.1f} cm" if s['empty_room'] is not None else "(not learned yet)"
    if s['still']:
        state = "STILL -> learning"
    elif not s['window_full']:
        state = "filling window"
    else:
        state = "moving"
    win_frac = (s['span'] / s['window']) if s['window'] else 0.0
    return [
        f"  smoothed {sm:>11}      velocity {vel:>11}",
        f"  window   [{_bar(win_frac)}] {s['span']:5.1f}/{s['window']:.0f} s  (n={s['samples']})",
        f"  motion   pp={s['pp']:5.2f} cm   speed={s['avg_speed']:5.3f}  vs thr {thr:.2f} cm/s   -> {state}",
        f"  EMPTY ROOM (full-destruction far): {er}    [live far = {s['far']:.1f} cm]",
    ]


def _env_num(key, default):
    try:
        return float(os.environ.get(key, default))
    except (TypeError, ValueError):
        return float(default)


class _TriggerMonitor:
    """Live mirror of server/src/inputs/daisy-position.js — the entry-bell +
    exit-voice + hysteretic-occupancy state machine the Node→Daisy bridge runs on
    the distance_cm / distance_velocity_cm_s / distance_far_cm feed. test_tof
    doesn't run the Node side, so this replicates it (defaults + env-var names
    match the JS) to show, live, when the bell would ring / the voice would speak
    — and, crucially, whether the room ever transitions occupied<->empty at all.
    Keep in sync with daisy-position.js if you retune either. (The periodic toll
    is omitted — irrelevant to entry/exit debugging.)"""

    def __init__(self):
        self.enter_frac = _env_num("BELL_ENTER_FRACTION", 0.15)
        self.empty_frac = _env_num("BELL_EMPTY_FRACTION", 0.08)
        self.rearm_recede_s = _env_num("BELL_REARM_RECEDE_S", 2.5)
        self.cooldown_s = _env_num("BELL_COOLDOWN_S", 30)
        self.approach_cm_s = _env_num("BELL_APPROACH_CM_S", 2.0)
        self.approach_sustain = max(1, int(_env_num("BELL_APPROACH_SUSTAIN", 3)))
        self.voice_dwell_s = _env_num("VOICE_PRESENCE_MIN_S", 3.0)
        self.voice_confirm_s = _env_num("VOICE_CONFIRM_EMPTY_S", 2.0)

        self.bell_armed = False
        self.empty_since = None
        self.approach_frames = 0
        self.last_bell_t = None
        self.room_occupied = False
        self.occupied_since = None
        self.voice_pending = False
        self.voice_empty_since = None

        self._t0 = None
        self.enter = None
        self.empty = None
        self.events = collections.deque(maxlen=8)

    def update(self, now, dist, vel, far, fresh=True):
        """Feed one frame: dist = smoothed distance_cm, vel = velocity (approach
        negative), far = learned far reach. Mirrors updateBellTrigger +
        updateVoiceTrigger; logs each arm/fire/occupancy transition."""
        if self._t0 is None:
            self._t0 = now
        if dist is None or not (far and far > 0):
            return
        vel = vel or 0.0
        enter = far * (1.0 - self.enter_frac)   # cross BELOW to trigger / occupy
        empty = far * (1.0 - self.empty_frac)   # recede ABOVE to re-arm / empty
        self.enter, self.empty = enter, empty

        # --- entry bell (updateBellTrigger) ---
        if dist >= empty:
            if self.empty_since is None:
                self.empty_since = now
            if not self.bell_armed and (now - self.empty_since) >= self.rearm_recede_s:
                self.bell_armed = True
                self.events.append((now, "bell ARMED (receded to empty)"))
            self.approach_frames = 0
        else:
            self.empty_since = None
            if fresh:
                if dist <= enter and (-vel) >= self.approach_cm_s:
                    self.approach_frames += 1
                else:
                    self.approach_frames = 0
            cooled = self.last_bell_t is None or (now - self.last_bell_t) >= self.cooldown_s
            if self.bell_armed and self.approach_frames >= self.approach_sustain and cooled:
                self.events.append((now, f"** BELL ** entry @ {dist:.0f}cm, {-vel:.1f}cm/s in"))
                self.bell_armed = False
                self.approach_frames = 0
                self.last_bell_t = now

        # --- occupancy + exit voice (updateVoiceTrigger) ---
        prev = self.room_occupied
        if dist <= enter:
            self.room_occupied = True
        elif dist >= empty:
            self.room_occupied = False
        if self.room_occupied != prev:
            self.events.append((now, f"occupancy -> {'OCCUPIED' if self.room_occupied else 'EMPTY'}"))

        if self.room_occupied:
            if self.occupied_since is None:
                self.occupied_since = now
            self.voice_empty_since = None
            if (now - self.occupied_since) >= self.voice_dwell_s:
                self.voice_pending = True
        else:
            self.occupied_since = None
            if self.voice_pending:
                if self.voice_empty_since is None:
                    self.voice_empty_since = now
                if (now - self.voice_empty_since) >= self.voice_confirm_s:
                    self.events.append((now, "** VOICE ** room emptied after a visit"))
                    self.voice_pending = False
                    self.voice_empty_since = None

    def lines(self, dist):
        d = f"{dist:.1f}" if dist is not None else "--"
        en = f"{self.enter:.0f}" if self.enter is not None else "--"
        em = f"{self.empty:.0f}" if self.empty is not None else "--"
        out = [
            f"  TRIGGERS  occupied={'YES' if self.room_occupied else 'no '}  "
            f"armed={'YES' if self.bell_armed else 'no '}  "
            f"approach={self.approach_frames}/{self.approach_sustain}  "
            f"voice_pending={'YES' if self.voice_pending else 'no'}",
            f"            dist={d:>7}cm   occupy≤{en}   empty≥{em}   "
            "(needs dist to swing across BOTH to fire entry/exit)",
        ]
        if self.events:
            out.append("  recent trigger events:")
            for (t, msg) in self.events:
                rel = (t - self._t0) if self._t0 is not None else 0.0
                out.append(f"    t+{rel:7.1f}s  {msg}")
        else:
            out.append("  recent trigger events: (none yet — walk into/out of the cone)")
        return out


_TUNE_HINT = (
    "  tune (config.py or env): EMPTY_ROOM_VELOCITY_CM_S, EMPTY_ROOM_STILLNESS_WINDOW_S,\n"
    "                           EMPTY_ROOM_MIN_CM, EMPTY_ROOM_RELEARN_S, EMPTY_ROOM_DOWN_ALPHA\n"
    "  empty the cone and hold still to learn the far reach; approach/wave to see it hold."
)


def _loop_l1x(backend) -> int:
    """Live dashboard: raw / mean / stddev / ambient PLUS the empty-room learner
    (velocity, stillness window fill, learned far reach). Reads run through the
    real DistanceDriver step, so what you see is what the kiosk computes."""
    sensor = backend._sensor
    drv = _make_driver(backend)
    mon = _TriggerMonitor()
    window = collections.deque(maxlen=50)  # ~1 s at 50 Hz, for the noise-floor sd
    last_print = 0.0
    last_d = None
    last_amb = None
    last_status = None
    sys.stdout.write("\033[2J")  # clear once; the loop repaints in place
    while True:
        if sensor.data_ready:
            d = sensor.distance  # cm, or None when no valid target in cone
            # Ambient + range status must be read after the measurement and
            # before clearing the interrupt, same ordering the driver uses.
            last_amb = backend._read_ambient_rate()
            last_status = getattr(sensor, "range_status", 0)
            sensor.clear_interrupt()
            last_d = d  # raw measured value, shown even when rejected below
            # Mirror _L1XBackend.read_raw: a read the sensor flags as unreliable
            # (status not in _VALID_STATUS — sigma/signal/wraparound) is dropped
            # to no-target so the learner sees the same filtered stream the kiosk
            # does. last_d still displays the raw number for tuning visibility.
            d_ok = d if (d is not None and last_status in backend._VALID_STATUS) else None
            sample_t = time.monotonic()
            drv._process_sample(d_ok, sample_t)
            mon.update(sample_t, drv._smoothed, drv._vel_ema, drv._far_cm)
            if d_ok is not None:
                window.append(d_ok)
        now = time.monotonic()
        if now - last_print >= 0.1:
            raw_s = f"{last_d:.1f} cm" if last_d is not None else "--"
            amb_s = f"{last_amb}" if last_amb is not None else "--"
            if last_status is None:
                st_s = "--"
            elif last_status in backend._VALID_STATUS:
                st_s = f"{last_status} ok"
            else:
                st_s = f"{last_status} REJECT"
            mu_s = f"{statistics.mean(window):.1f} cm" if len(window) >= 2 else "--"
            sd_s = f"{statistics.stdev(window):.2f}" if len(window) >= 2 else "--"
            # Repaint in place: home cursor + clear below (same trick as L5CX).
            sys.stdout.write("\033[H\033[J")
            print(f"[VL53L1X]  far ceiling = {backend.far_cm:.0f} cm    "
                  f"ambient = {amb_s} (tune VL53_AMBIENT_LONG_MAX, projector ON, real wall)")
            print(f"  raw {raw_s:>11}   mean(1s) {mu_s:>11}   sd {sd_s:>6}   "
                  f"status {st_s:>9}   n={len(window)}")
            print()
            for ln in _empty_room_lines(drv, now):
                print(ln)
            print()
            for ln in mon.lines(drv._smoothed):
                print(ln)
            print()
            print(_TUNE_HINT)
            print("  ctrl-c to exit", flush=True)
            last_print = now
        time.sleep(0.005)


def _loop_l5cx(backend) -> int:
    """Closest valid zone + a live distance grid (cm per zone, '--' = invalid)."""
    sensor = backend._sensor
    n = backend._grid_n
    side = 8 if n == 64 else 4
    drv = _make_driver(backend)
    mon = _TriggerMonitor()
    print(f"\nclosest = nearest valid zone in the cone (what publishes as distance_cm)")
    print("grid shows per-zone distance in cm ('--' = no valid target this frame)")
    print("ctrl-c to exit\n")

    last_print = 0.0
    while True:
        if sensor.data_ready():
            data = sensor.get_data()
            # 2D ctypes arrays: [target][zone]; target 0 = closest per zone (see
            # _L5CXBackend.read_raw). Inner dim is the full 64-zone buffer.
            dists = data.distance_mm[0]
            stats = data.target_status[0]
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
            closest = min(valid_cm) if valid_cm else None
            # Feed the reduced closest-zone distance (None = no-target) through
            # the real driver step so the empty-room learner runs live.
            drv._process_sample(closest, now)
            mon.update(now, drv._smoothed, drv._vel_ema, drv._far_cm)
            if now - last_print >= 0.25:
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
                      f"far ceiling={backend.far_cm:.0f}cm\n")
                print(rows + "\n")
                for ln in _empty_room_lines(drv, now):
                    print(ln)
                print()
                for ln in mon.lines(drv._smoothed):
                    print(ln)
                print()
                print(_TUNE_HINT, flush=True)
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
