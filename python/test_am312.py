#!/usr/bin/env python3
"""Combined AM312 motion + VL53 distance presence bringup / simulation tool.

The companion to `test_tof.py`. Where test_tof focuses on tuning the ToF feed,
this shows the COMBINED picture the Node→Daisy bridge actually acts on: the two
AM312 PIRs (OR'd into one `motion` signal) fused with the ToF distance feed into
one room-occupancy, and the entry-bell / exit-voice triggers that ride on it.

It is a faithful Python mirror of the fused state machine in
`server/src/inputs/daisy-position.js` (MOTION_PRESENCE / computeOccupancy /
motion-mode bell / distance fallback). test_am312 doesn't run the Node side, so
it replicates it — defaults and env-var names match the JS. Keep in sync if you
retune either. (The periodic toll is omitted — irrelevant to entry/exit/occupancy
debugging, same as test_tof's monitor.)

Three modes:

  python test_am312.py                 # --scenario: deterministic timeline + event
                                       #   log, run BOTH motion-ON and motion-OFF so
                                       #   you can see exactly what motion adds. No
                                       #   hardware — runs on the Mac.
  python test_am312.py --mock          # synthetic motion + distance from the real
                                       #   drivers' own mock loops; live dashboard.
  python test_am312.py --live          # real AM312 GPIOs (per-sensor) + real ToF;
                                       #   the Pi bringup view. Ctrl-C to exit.

Motion fusion is ON by default in this tool (the whole point is testing it);
production defaults MOTION_PRESENCE OFF. Override per run with `--motion off`
(falls back to distance-only, same as the kiosk without the flag). Env knobs
honored (match the JS): MOTION_HOLD_S, BELL_ENTER_FRACTION, BELL_EMPTY_FRACTION,
BELL_REARM_RECEDE_S, BELL_COOLDOWN_S, BELL_APPROACH_CM_S, BELL_APPROACH_SUSTAIN,
VOICE_PRESENCE_MIN_S, VOICE_CONFIRM_EMPTY_S.

NOTE: this tool does NOT apply the AM312's 60 s boot suppression — you want to
see the sensors toggle immediately during bringup. The kiosk suppresses motion
for 60 s post-boot (config.PIR_BOOT_SUPPRESS_S).
"""

import argparse
import collections
import logging
import os
import sys
import time

# Run from python/ so the package imports: `cd python && python test_am312.py`.
from ambient_kiosk import config
from ambient_kiosk.sensors.distance import DistanceDriver


def _env_num(key, default):
    try:
        return float(os.environ.get(key, default))
    except (TypeError, ValueError):
        return float(default)


def _env_bool(key, default):
    v = os.environ.get(key)
    if v is None:
        return default
    return v.strip().lower() in ("1", "true", "yes", "on")


class CombinedTriggerMonitor:
    """Faithful mirror of server/src/inputs/daisy-position.js with motion fusion.

    Fed three ways, exactly as the JS onChange does:
      feed_motion(now, on)             -> motion edge   (updateTriggers fresh=False)
      feed_distance(now, d, v, far)    -> distance frame (updateTriggers fresh=True)
      tick(now)                        -> slow timer     (updateTriggers fresh=False)

    Every feed runs computeOccupancy() then the bell + voice triggers, and logs
    each arm / strike / occupancy transition with a timestamp.
    """

    def __init__(self, motion_presence, hold_s=None, log_cap=None):
        # Thresholds + dwell — names/defaults mirror the JS.
        self.enter_frac = _env_num("BELL_ENTER_FRACTION", 0.15)
        self.empty_frac = _env_num("BELL_EMPTY_FRACTION", 0.08)
        self.rearm_recede_s = _env_num("BELL_REARM_RECEDE_S", 2.5)
        self.cooldown_s = _env_num("BELL_COOLDOWN_S", 30)
        self.approach_cm_s = _env_num("BELL_APPROACH_CM_S", 2.0)
        self.approach_sustain = max(1, int(_env_num("BELL_APPROACH_SUSTAIN", 3)))
        self.voice_dwell_s = _env_num("VOICE_PRESENCE_MIN_S", 3.0)
        self.voice_confirm_s = _env_num("VOICE_CONFIRM_EMPTY_S", 2.0)

        # Motion fusion config (the part test_tof's monitor lacks).
        self.motion_presence = motion_presence
        self.hold_s = _env_num("MOTION_HOLD_S", 20) if hold_s is None else hold_s

        # Live inputs.
        self.dist = None
        self.vel = 0.0
        self.far = None
        self.motion_active = False
        self.last_motion_t = float("-inf")

        # Fused occupancy + bell + voice state (1:1 with the JS module vars).
        self.room_occupied = False
        self.bell_armed = False
        self.empty_since = None
        self.approach_frames = 0
        self.last_bell_t = None
        self.occupied_since = None
        self.voice_pending = False
        self.voice_empty_since = None

        # Derived display values.
        self.enter = None
        self.empty = None
        self.occupancy_reason = "--"

        self._t0 = None
        self.events = collections.deque(maxlen=log_cap) if log_cap else []

    # --- logging -----------------------------------------------------------
    def _log(self, now, msg):
        if self._t0 is None:
            self._t0 = now
        self.events.append((now, msg))

    def mark(self, now, msg):
        """Scenario scene marker, interleaved with triggers in the event log."""
        self._log(now, f". {msg}")

    # --- the JS motionPresent() -------------------------------------------
    def _motion_present(self, now):
        if not self.motion_presence:
            return False
        return self.motion_active or (now - self.last_motion_t) <= self.hold_s

    # --- feeds -------------------------------------------------------------
    def feed_motion(self, now, on):
        if self._t0 is None:
            self._t0 = now
        on = bool(on)
        # Log the OR'd motion edge for visibility.
        if on != self.motion_active:
            self._log(now, f"motion -> {'MOTION' if on else 'quiet'}")
        self.motion_active = on
        self.last_motion_t = now  # stamp every edge; hold measures from the fall
        self._run(now, fresh=False)

    def feed_distance(self, now, dist, vel, far, fresh=True):
        if self._t0 is None:
            self._t0 = now
        self.dist = dist
        self.vel = vel or 0.0
        if far and far > 0:
            self.far = far
        self._run(now, fresh=fresh)

    def tick(self, now):
        if self._t0 is None:
            self._t0 = now
        self._run(now, fresh=False)

    # --- the JS updateTriggers (computeOccupancy -> bell -> voice) ---------
    def _run(self, now, fresh):
        self._compute_occupancy(now)
        self._update_bell(now, fresh)
        self._update_voice(now)

    def _compute_occupancy(self, now):
        prev = self.room_occupied
        reason = "empty"
        if self.dist is not None and self.far and self.far > 0:
            self.enter = self.far * (1.0 - self.enter_frac)  # near -> occupied
            self.empty = self.far * (1.0 - self.empty_frac)  # far  -> empty
            if self.dist <= self.enter:
                self.room_occupied = True
                reason = "distance-near"
            elif self.dist >= self.empty:
                self.room_occupied = False
        # Motion fusion (opt-in): augment-only — forces occupied, never clears.
        if self._motion_present(now):
            self.room_occupied = True
            reason = "motion-held" if not self.motion_active else "motion"
        self.occupancy_reason = reason if self.room_occupied else "empty"
        if self.room_occupied != prev:
            self._log(now, f"occupancy -> {'OCCUPIED' if self.room_occupied else 'EMPTY'}"
                           f" ({self.occupancy_reason})")

    def _update_bell(self, now, fresh):
        if self.motion_presence:
            self._update_bell_motion(now)
            return
        # --- distance fallback path (unchanged from the kiosk's off-mode) ---
        if self.dist is None or not (self.far and self.far > 0):
            return
        if self.dist >= self.empty:
            if self.empty_since is None:
                self.empty_since = now
            if not self.bell_armed and (now - self.empty_since) >= self.rearm_recede_s:
                self.bell_armed = True
                self._log(now, "bell ARMED (receded to empty)")
            self.approach_frames = 0
            return
        self.empty_since = None
        if fresh:
            if self.dist <= self.enter and (-self.vel) >= self.approach_cm_s:
                self.approach_frames += 1
            else:
                self.approach_frames = 0
        cooled = self.last_bell_t is None or (now - self.last_bell_t) >= self.cooldown_s
        if self.bell_armed and self.approach_frames >= self.approach_sustain and cooled:
            self._log(now, f"** BELL ** entry (distance) @ {self.dist:.0f}cm, "
                           f"{-self.vel:.1f}cm/s in")
            self.bell_armed = False
            self.approach_frames = 0
            self.last_bell_t = now

    def _update_bell_motion(self, now):
        # Motion-mode bell: fire on the empty->occupied edge, after an armed empty
        # hold so a person already present at boot doesn't ring. roomOccupied is
        # set by _compute_occupancy above.
        if not self.room_occupied:
            if self.empty_since is None:
                self.empty_since = now
            if not self.bell_armed and (now - self.empty_since) >= self.rearm_recede_s:
                self.bell_armed = True
                self._log(now, "bell ARMED (room empty)")
            return
        self.empty_since = None
        cooled = self.last_bell_t is None or (now - self.last_bell_t) >= self.cooldown_s
        if self.bell_armed and cooled:
            src = "motion" if self.motion_active else "motion-mode, ToF"
            self._log(now, f"** BELL ** entry ({src})")
            self.bell_armed = False
            self.last_bell_t = now

    def _update_voice(self, now):
        # Reads the occupancy _compute_occupancy already set.
        if self.room_occupied:
            if self.occupied_since is None:
                self.occupied_since = now
            self.voice_empty_since = None
            if (now - self.occupied_since) >= self.voice_dwell_s:
                self.voice_pending = True
            return
        self.occupied_since = None
        if not self.voice_pending:
            return
        if self.voice_empty_since is None:
            self.voice_empty_since = now
        if (now - self.voice_empty_since) >= self.voice_confirm_s:
            self._log(now, "** VOICE ** room emptied after a visit")
            self.voice_pending = False
            self.voice_empty_since = None

    # --- display -----------------------------------------------------------
    def status_lines(self):
        d = f"{self.dist:.1f}" if self.dist is not None else "--"
        en = f"{self.enter:.0f}" if self.enter is not None else "--"
        em = f"{self.empty:.0f}" if self.empty is not None else "--"
        far = f"{self.far:.0f}" if self.far is not None else "--"
        return [
            f"  FUSED  occupied={'YES' if self.room_occupied else 'no '} ({self.occupancy_reason:<13}) "
            f"armed={'YES' if self.bell_armed else 'no '}  "
            f"approach={self.approach_frames}/{self.approach_sustain}  "
            f"voice_pending={'YES' if self.voice_pending else 'no'}",
            f"         dist={d:>7}cm  vel={self.vel:+5.1f}  far={far}cm  "
            f"occupy<={en}  empty>={em}",
        ]

    def log_lines(self, recent=None):
        evs = list(self.events)
        if recent is not None:
            evs = evs[-recent:]
        if not evs:
            return ["  events: (none yet)"]
        out = ["  events:"]
        for (t, msg) in evs:
            rel = (t - self._t0) if self._t0 is not None else 0.0
            out.append(f"    t+{rel:7.2f}s  {msg}")
        return out


# ===========================================================================
# Scenario mode — deterministic timeline, run motion-ON and motion-OFF.
# ===========================================================================

# Simulated VL53L1X short-mode far reach (cm). enter≈110.5, empty≈119.6.
_SCN_FAR = 130.0

# (t_start, motion_level, dist_cm, vel_cm_s, scene marker). Piecewise-held:
# each field persists until the next segment changes it. The marker is logged
# once when the segment goes active.
_SCENARIO = [
    (0.0,  False, 128.0,  0.0, "room empty; entry bell arming"),
    (8.0,  True,  128.0,  0.0, "visitor enters the ROOM — AM312 sees motion, ToF cone still empty"),
    (16.0, True,  95.0,  -6.0, "visitor approaches the KIOSK — ToF now reads near, moving in"),
    (20.0, False, 128.0,  6.0, "visitor leaves — motion stops, ToF reads empty"),
    # (motion-ON holds occupancy for MOTION_HOLD_S after the t=20 fall, then empties)
]
_SCN_END = 46.0
_SCN_DT = 0.5


def _active_segment(t):
    seg = _SCENARIO[0]
    for s in _SCENARIO:
        if s[0] <= t:
            seg = s
        else:
            break
    return seg


def _run_scenario(motion_presence, hold_s=None):
    mon = CombinedTriggerMonitor(motion_presence=motion_presence, hold_s=hold_s)
    # Resting state is "quiet" — like production, the OR'd driver publishes only
    # on a change, so the first motion event is a real rising edge (never an
    # initial False, which would falsely stamp the hold window from t=0).
    prev_motion = False
    prev_marker = None
    t = 0.0
    while t <= _SCN_END + 1e-9:
        _, m_level, dist, vel, marker = _active_segment(t)
        if marker != prev_marker:
            mon.mark(t, marker)
            prev_marker = marker
        # Motion edge only (not every tick — else the hold would never start).
        if m_level != prev_motion:
            mon.feed_motion(t, m_level)
            prev_motion = m_level
        # A fresh distance frame each tick (the approach gate needs fresh samples);
        # ticks with no distance change still advance time-based transitions.
        mon.feed_distance(t, dist, vel, _SCN_FAR, fresh=True)
        t += _SCN_DT
    return mon


def _scenario_main(args):
    hold = args.hold
    print("=" * 74)
    print("SCENARIO: empty room -> visitor enters room (off ToF) -> approaches kiosk")
    print("          -> leaves. Same timeline, fused motion+distance vs distance-only.")
    print(f"          far={_SCN_FAR:.0f}cm  MOTION_HOLD_S={hold if hold is not None else _env_num('MOTION_HOLD_S', 20):.0f}")
    print("=" * 74)
    for label, mp in (("MOTION ON  (MOTION_PRESENCE=1, AM312 fused)", True),
                      ("MOTION OFF (distance-only fallback)", False)):
        print(f"\n--- {label} ---")
        mon = _run_scenario(mp, hold_s=hold)
        for ln in mon.log_lines():
            print(ln)
    print("\nRead the two logs together: with motion ON the bell rings when the visitor")
    print("enters the ROOM (t≈8, off the ToF cone) and the voice waits out the motion")
    print("hold after they leave; with it OFF the bell only rings on the KIOSK approach")
    print("(t≈17) and the voice fires right after they step out of the cone.")
    return 0


# ===========================================================================
# Live / mock mode — real (or mock) drivers feed the same monitor.
# ===========================================================================

class _CaptureIngest:
    """Records the latest value per channel; what the live dashboard reads."""
    def __init__(self):
        self.vals = {}

    def publish(self, name, value):
        self.vals[name] = value


def _open_am312_sensors():
    """Live per-sensor AM312 read (direct gpiozero, for bringup visibility).
    Returns (sensors, errors): sensors = list of (pin, MotionSensor); errors =
    list of human-readable reasons for any pin that failed (shown in the
    dashboard so the cause isn't lost to the screen repaint)."""
    errors = []
    try:
        from gpiozero import MotionSensor  # lazy: Pi-only
    except Exception as e:  # ImportError, or BadPinFactory at import time
        return [], [f"gpiozero import failed: {type(e).__name__}: {e}"]
    out = []
    for pin in config.PIR_PINS:
        try:
            # pull_down: AM312 OUT idles LOW and is driven HIGH on motion. An
            # unconnected pin floats — explicit pull-down makes a missing/loose
            # signal read a steady LOW rather than a random toggle.
            out.append((pin, MotionSensor(pin, pull_up=False)))
        except Exception as e:
            msg = f"BCM{pin}: {type(e).__name__}: {e}"
            # 'GPIO busy' = another process owns the line (lgpio cdev). The usual
            # culprit is the running kiosk sidecar, which claims these same pins.
            if "busy" in str(e).lower():
                msg += "  -> pin already in use; stop the kiosk (pkill -f ambient_kiosk)"
            errors.append(msg)
    return out, errors


def _live_main(args):
    mock = args.mock
    mon = CombinedTriggerMonitor(motion_presence=args.motion, hold_s=args.hold, log_cap=10)
    cap = _CaptureIngest()

    # ToF: the REAL driver (its own thread does all the ToF + empty-room work and
    # publishes distance_cm / velocity / far). No read loop duplicated here.
    dist_drv = DistanceDriver(cap, mock=mock)
    dist_drv.start()

    # AM312: mock -> the drivers' synthetic motion via PirDriver; live -> direct
    # per-sensor GPIO reads so you can see each unit toggle.
    pir_drv = None
    sensors = []
    pir_errors = []
    if mock:
        from ambient_kiosk.sensors.pir import PirDriver
        config.PIR_BOOT_SUPPRESS_S = 0.0  # no 60 s wait for a demo
        pir_drv = PirDriver(cap, mock=True)
        pir_drv.start()
    else:
        sensors, pir_errors = _open_am312_sensors()
        if not sensors:
            print("no AM312 sensors initialised — check PIR_PINS / wiring:", file=sys.stderr)
            for err in (pir_errors or ["(no pins configured in PIR_PINS)"]):
                print(f"  {err}", file=sys.stderr)

    print(f"\n[test_am312] {'MOCK' if mock else 'LIVE'}  "
          f"motion fusion {'ON' if args.motion else 'OFF (distance-only)'}  "
          f"hold={mon.hold_s:.0f}s  pins={config.PIR_PINS}")
    print("production default is MOTION_PRESENCE OFF; no 60 s boot suppression here.")
    print("ctrl-c to exit\n")

    last_dist = object()  # sentinel != any float
    last_motion = None
    last_tick = 0.0
    last_paint = 0.0
    sys.stdout.write("\033[2J")
    try:
        while True:
            now = time.monotonic()

            # OR'd motion: live = any per-sensor; mock = the published channel.
            if mock:
                motion_or = bool(cap.vals.get("motion", False))
                per = None
            else:
                states = [bool(s.motion_detected) for (_p, s) in sensors]
                motion_or = any(states)
                per = states
            if motion_or != last_motion:
                mon.feed_motion(now, motion_or)
                last_motion = motion_or

            # Distance on change (fresh), plus a 2 Hz tick for time progression.
            dist = cap.vals.get("distance_cm")
            vel = cap.vals.get("distance_velocity_cm_s", 0.0) or 0.0
            far = cap.vals.get("distance_far_cm") or getattr(dist_drv, "_far_cm", config.VL53_FAR_CM)
            if dist != last_dist:
                mon.feed_distance(now, dist, vel, far, fresh=True)
                last_dist = dist
            if now - last_tick >= 0.5:
                mon.tick(now)
                last_tick = now

            if now - last_paint >= 0.2:
                sys.stdout.write("\033[H\033[J")
                print(f"[test_am312 {'MOCK' if mock else 'LIVE'}]  "
                      f"motion fusion {'ON' if args.motion else 'OFF'}  hold={mon.hold_s:.0f}s")
                # AM312 line.
                hold_left = max(0.0, mon.hold_s - (now - mon.last_motion_t)) if mon.last_motion_t != float("-inf") else 0.0
                if mock:
                    am = f"OR={'MOTION' if motion_or else 'quiet '} (mock)"
                elif sensors:
                    am = "  ".join(f"BCM{p}={'#' if st else '.'}"
                                   for (p, _s), st in zip(sensors, per))
                    am += f"   OR={'MOTION' if motion_or else 'quiet '}"
                else:
                    # No sensors opened — keep the reason on screen (it would
                    # otherwise be wiped by the repaint).
                    am = "(none) " + (" | ".join(pir_errors) if pir_errors else "no PIR_PINS")
                print(f"  AM312   {am}   hold_left={hold_left:4.1f}s")
                print()
                for ln in mon.status_lines():
                    print(ln)
                print()
                for ln in mon.log_lines(recent=10):
                    print(ln)
                print("\n  ctrl-c to exit", flush=True)
                last_paint = now

            time.sleep(0.01)
    except KeyboardInterrupt:
        print()
        return 0
    finally:
        try:
            dist_drv.stop()
        except Exception:
            pass
        if pir_drv is not None:
            try:
                pir_drv.stop()
            except Exception:
                pass
        for (_p, s) in sensors:
            try:
                s.close()
            except Exception:
                pass


def main() -> int:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    p = argparse.ArgumentParser(prog="test_am312", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    g = p.add_mutually_exclusive_group()
    g.add_argument("--scenario", action="store_true",
                   help="deterministic timeline + event log (default; no hardware)")
    g.add_argument("--mock", action="store_true",
                   help="synthetic motion + distance from the real drivers' mock loops")
    g.add_argument("--live", action="store_true",
                   help="real AM312 GPIOs + real ToF (Pi bringup)")
    p.add_argument("--motion", choices=("on", "off"), default=None,
                   help="motion fusion (default on for this tool; production default off)")
    p.add_argument("--hold", type=float, default=None,
                   help="MOTION_HOLD_S override (s) for this run")
    args = p.parse_args()

    # Motion fusion: default ON for the tool, honor explicit flag, then env.
    if args.motion is None:
        args.motion = _env_bool("MOTION_PRESENCE", True)
    else:
        args.motion = args.motion == "on"

    if args.live:
        return _live_main(args)
    if args.mock:
        return _live_main(args)
    return _scenario_main(args)


if __name__ == "__main__":
    sys.exit(main())
