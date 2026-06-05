"""Time-of-flight distance sensing over I²C.

Supports two interchangeable ST ToF sensors behind a common backend
interface (selected by `VL53_SENSOR`, or auto-detected — both default to
I²C address 0x29 but report distinct model IDs):

  - VL53L1X  — single-point ToF, short/long distance modes, ambient-IR
               auto-mode select. The original kiosk sensor.
  - VL53L5CX — multizone (4x4 / 8x8) ToF; the zone grid is reduced to one
               distance by taking the closest valid zone in the cone.

Either way the driver polls at ~50 Hz, smooths with an EMA, and publishes
a single `distance_cm` plus the sensor's far reach as `distance_far_cm`.
Invalid reads (no target in cone) snap toward `far` rather than freezing.
"""

import logging
import threading
import time
from typing import Optional

from .. import config

log = logging.getLogger(__name__)


def _open_i2c():
    """Open the shared I²C bus. Raises if hardware/libs are unavailable."""
    import board
    import busio
    return busio.I2C(board.SCL, board.SDA)


class _L1XBackend:
    """VL53L1X single-point ToF. Behaviour unchanged from the original
    single-sensor driver: ambient-IR sample picks short vs long mode, and the
    timing budget + far reach follow the chosen mode."""

    name = "VL53L1X"

    # VL53L1X result register holding the per-measurement ambient IR count
    # rate (ST ULD name: VL53L1_RESULT__AMBIENT_COUNT_RATE_MCPS_SD0). The
    # Adafruit CircuitPython lib doesn't surface this, so we read it raw.
    # Only valid after a completed measurement (data_ready).
    _REG_AMBIENT_RATE = 0x0090

    # ST ULD range-status values we treat as a usable read. 0 = range valid (no
    # error). Everything else is the sensor flagging the distance as unreliable:
    # 1 sigma fail (noisy), 2 signal fail (return too weak), 4 out of bounds,
    # 7 wraparound (a far/specular target aliased to a phantom, usually a
    # too-close ghost). These dominate in long mode under projector IR and are
    # exactly the spurious "valid-looking" reads that poison the empty-room
    # learner, so we drop them to no-target. Widen this set if strict 0-only
    # ends up rejecting good reads on the real wall.
    _VALID_STATUS = (0,)

    def __init__(self, sensor):
        self._sensor = sensor
        self.far_cm: float = config.VL53_FAR_CM  # resolved in configure()

    @staticmethod
    def probe(i2c) -> Optional["_L1XBackend"]:
        """Construct the L1X driver. The Adafruit constructor reads the model
        ID and raises on anything that isn't a VL53L1X (0xEACC) *before* it
        writes any init sequence, so this doubles as a non-destructive probe:
        returns the backend on a real L1X, or None otherwise."""
        try:
            import adafruit_vl53l1x
            sensor = adafruit_vl53l1x.VL53L1X(i2c, address=config.VL53L1X_ADDR)
        except Exception as e:
            log.debug("distance: not a VL53L1X (%s)", e)
            return None
        return _L1XBackend(sensor)

    @staticmethod
    def _budget_for_mode(mode: int) -> int:
        """Timing budget (ms) valid for the given distance mode. Long mode needs
        a much larger budget to reach 4 m (20 ms is short-mode-only)."""
        return (config.VL53_TIMING_BUDGET_MS_LONG if mode == 2
                else config.VL53_TIMING_BUDGET_MS_SHORT)

    @staticmethod
    def _far_for_mode(mode: int) -> float:
        """Far reach (cm) for the given distance mode."""
        return config.VL53_FAR_CM_LONG if mode == 2 else config.VL53_FAR_CM_SHORT

    def configure(self) -> bool:
        try:
            # Start in short mode for the ambient sample: it's the ambient-safe
            # fallback we stay in if auto-select decides the scene is too bright,
            # so there's no glitch if we don't switch.
            mode = 1 if config.VL53_AUTO_MODE else config.VL53_DISTANCE_MODE
            self._sensor.distance_mode = mode
            # Auto-select samples ambient in short mode, so start at the short
            # budget; the final mode's budget is applied below / in _calibrate.
            self._sensor.timing_budget = (config.VL53_TIMING_BUDGET_MS
                                          if config.VL53_AUTO_MODE
                                          else self._budget_for_mode(mode))
            self._sensor.start_ranging()
            if config.VL53_AUTO_MODE:
                mode = self._calibrate_distance_mode(mode)
            self.far_cm = self._far_for_mode(mode)
            log.info("distance: VL53L1X ready (mode=%d, budget=%dms, far=%.0fcm)",
                     mode, self._budget_for_mode(mode), self.far_cm)
            return True
        except Exception as e:
            log.error("distance: VL53L1X configure failed: %s", e)
            return False

    def _read_ambient_rate(self) -> Optional[int]:
        """Raw ambient IR count rate from the last measurement, or None.

        Units follow ST's ULD convention (register word * 8). The absolute
        scale is only meaningful relative to an on-site dark baseline — which
        is the point: we log it so VL53_AMBIENT_LONG_MAX can be tuned for the
        real room + projector. Reaches past the Adafruit API via its private
        register helper, so it degrades gracefully if that ever changes.
        """
        try:
            raw = self._sensor._read_register(self._REG_AMBIENT_RATE, 2)
            return ((raw[0] << 8) | raw[1]) * 8
        except Exception as e:
            log.debug("distance: ambient read unavailable: %s", e)
            return None

    def _calibrate_distance_mode(self, current_mode: int) -> int:
        """Sample ambient IR for VL53_AMBIENT_CAL_S, return the chosen mode.

        Low ambient -> long mode (reach to ~4 m). High ambient (a bright lamp
        projector throwing 940 nm onto the scene the sensor faces) -> short
        mode, which tolerates ambient far better. Keeps current_mode if the
        sensor never yields a usable ambient sample.
        """
        samples = []
        deadline = time.monotonic() + config.VL53_AMBIENT_CAL_S
        while time.monotonic() < deadline:
            try:
                if self._sensor.data_ready:
                    amb = self._read_ambient_rate()
                    self._sensor.clear_interrupt()
                    if amb is not None:
                        samples.append(amb)
            except Exception:
                pass
            time.sleep(0.01)

        if not samples:
            log.warning("distance: ambient calibration got no samples; "
                        "keeping mode=%d", current_mode)
            return current_mode

        samples.sort()
        median = samples[len(samples) // 2]
        chosen = 2 if median <= config.VL53_AMBIENT_LONG_MAX else 1
        log.info("distance: ambient median=%d over %d samples (long_max=%d) -> %s mode",
                 median, len(samples), config.VL53_AMBIENT_LONG_MAX,
                 "long" if chosen == 2 else "short")
        if chosen != current_mode:
            # Mode switch must bracket a ranging stop; apply the chosen mode's
            # timing budget — long mode needs ≥140 ms to reach 4 m, whereas the
            # 20 ms short budget is invalid for long mode entirely.
            self._sensor.stop_ranging()
            self._sensor.distance_mode = chosen
            self._sensor.timing_budget = self._budget_for_mode(chosen)
            self._sensor.start_ranging()
        return chosen

    def read_raw(self) -> Optional[float]:
        """Returns distance in cm, or None for no-target / invalid.

        A read the sensor flags as unreliable (range_status not in
        _VALID_STATUS) is treated as no-target: in long mode under ambient IR
        the VL53L1X emits sigma/signal/wraparound phantoms that look like valid
        large distances and would otherwise poison the empty-room learner.
        """
        try:
            if not self._sensor.data_ready:
                return None
            d = self._sensor.distance  # cm; None if no valid target
            # Range status is per-measurement; read it before clearing the
            # interrupt (same ordering as the ambient read). getattr guards
            # older lib versions that don't surface it (treated as valid=0).
            status = getattr(self._sensor, "range_status", 0)
            self._sensor.clear_interrupt()
            if d is None or status not in self._VALID_STATUS:
                return None
            return float(d)
        except Exception as e:
            log.debug("distance: read error: %s", e)
            return None

    def stop(self) -> None:
        try:
            self._sensor.stop_ranging()
        except Exception:
            pass


class _L5CXBackend:
    """VL53L5CX multizone ToF. No short/long mode — a single ~4 m ranging
    range. The 4x4 / 8x8 grid is reduced to one distance by taking the closest
    valid zone within the configured cone window (edge zones graze the
    wall/floor, so a sub-window can be selected via VL53L5CX_CONE_ZONES).

    NOTE: the library calls below target Pimoroni's `vl53l5cx_ctypes`. If you
    install a different binding, adjust the method/attribute names here — the
    rest of the pipeline only needs read_raw() -> Optional[cm] and far_cm.
    """

    name = "VL53L5CX"

    # ST per-zone target_status values we treat as a usable range. 5 = range
    # valid (good); 9 = valid but with reduced confidence (wraparound-checked).
    # Anything else (0/255 not updated, 4 phase fail, etc.) is ignored.
    _VALID_STATUS = (5, 9)

    def __init__(self, sensor):
        self._sensor = sensor
        self._grid_n = config.VL53L5CX_RESOLUTION  # 16 (4x4) or 64 (8x8)
        self.far_cm: float = config.VL53L5CX_FAR_CM

    @staticmethod
    def probe(i2c) -> Optional["_L5CXBackend"]:
        """Construct the L5CX driver. Its constructor checks the device is
        alive and uploads the ~84 KB firmware blob, raising if no L5CX is
        present — so this is the probe. (i2c is accepted for symmetry; the
        ctypes lib opens its own handle.)"""
        try:
            import vl53l5cx_ctypes as vl53l5cx  # noqa: F401  (verify your lib)
        except Exception as e:
            log.debug("distance: VL53L5CX lib unavailable (%s)", e)
            return None
        try:
            sensor = vl53l5cx.VL53L5CX()
        except Exception as e:
            log.debug("distance: not a VL53L5CX (%s)", e)
            return None
        return _L5CXBackend(sensor)

    def configure(self) -> bool:
        try:
            self._sensor.set_resolution(self._grid_n)
            self._sensor.set_ranging_frequency_hz(config.VL53L5CX_RANGING_HZ)
            self._sensor.start_ranging()
            side = 8 if self._grid_n == 64 else 4
            log.info("distance: VL53L5CX ready (%dx%d @ %dHz, far=%.0fcm)",
                     side, side, config.VL53L5CX_RANGING_HZ, self.far_cm)
            return True
        except Exception as e:
            log.error("distance: VL53L5CX configure failed: %s", e)
            return False

    def read_raw(self) -> Optional[float]:
        """Reduce the zone grid to the closest valid distance (cm) in the cone,
        or None if no zone has a usable target this frame."""
        try:
            if not self._sensor.data_ready():
                return None
            data = self._sensor.get_data()
            # Pimoroni's results are 2D ctypes arrays indexed [target][zone]:
            # the outer dim is nb_target_per_zone (1 in our config), the inner is
            # the full 64-zone (8x8) buffer of which the first `_grid_n` are
            # populated at the set resolution. Take target 0 — the closest /
            # strongest per zone — then reduce to the nearest valid zone below.
            dists = data.distance_mm[0]    # 64 per-zone mm (target 0)
            stats = data.target_status[0]  # 64 per-zone status (target 0)
            n = min(self._grid_n, len(dists), len(stats))
            zones = config.VL53L5CX_CONE_ZONES
            if not zones:
                zones = range(n)
            best = None
            for z in zones:
                if z >= n:
                    continue
                if stats[z] in self._VALID_STATUS and dists[z] > 0:
                    cm = dists[z] / 10.0
                    if best is None or cm < best:
                        best = cm
            return best
        except Exception as e:
            log.debug("distance: L5CX read error: %s", e)
            return None

    def stop(self) -> None:
        try:
            self._sensor.stop_ranging()
        except Exception:
            pass


def _make_backend(i2c):
    """Select a sensor backend per VL53_SENSOR ('auto' | 'l1x' | 'l5cx').

    In 'auto' we probe the L1X first: that probe is cheap and non-destructive
    (model-ID read, no firmware upload), so we only fall through to the L5CX —
    which uploads its firmware blob — when the L1X isn't the one wired."""
    want = (getattr(config, "VL53_SENSOR", "auto") or "auto").lower()
    if want == "l1x":
        return _L1XBackend.probe(i2c)
    if want == "l5cx":
        return _L5CXBackend.probe(i2c)
    # auto
    backend = _L1XBackend.probe(i2c)
    if backend is not None:
        return backend
    return _L5CXBackend.probe(i2c)


class DistanceDriver:
    # Seconds with no valid read before we snap to the far reach. Long enough to
    # ride out multi-frame dropouts (dark clothing, an oblique torso, projector
    # IR) that would otherwise flicker a present visitor to "empty"; the cost is
    # that a genuine walk-away takes this long to register as idle. Presence
    # favours ride-out over snappiness, so the default sits above the old 0.6 s.
    # Env-overridable via NO_TARGET_TIMEOUT_S (see config.py).
    NO_TARGET_TIMEOUT_S = config.NO_TARGET_TIMEOUT_S

    def __init__(self, ingest, mock: bool = False):
        self.ingest = ingest
        self.mock = mock
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._backend = None
        self._i2c = None
        self._smoothed: Optional[float] = None
        self._last_published: Optional[float] = None
        # Wall-clock of the last valid (non-None) read. Stays 0.0 until the
        # first one arrives, so the initial published value is FAR (idle).
        self._last_valid_t: float = 0.0
        # Sensor's far reach in cm — the no-target snap value and the saturation
        # end of the downstream effect mappings. Starts at the sensor's configured
        # reach and is then driven by the learned empty-room distance (see below).
        # Resolved once the backend is configured (short-mode default until then,
        # also used in mock mode).
        self._far_cm: float = config.VL53_FAR_CM
        # Hard ceiling for the learned far reach — the sensor's reliable max,
        # captured at configure() time. The empty-room estimate can move freely
        # below this but never beyond it (readings past the sensor's reach are
        # unreliable). Stays at the config default in mock mode.
        self._far_ceiling: float = config.VL53_FAR_CM

        # --- Empty-room learning state -------------------------------------
        # Trailing window of (monotonic_t, smoothed_cm) from VALID reads only,
        # used to gauge stillness via peak-to-peak excursion over the window.
        self._dist_hist: list = []
        # When the current run of continuous valid reads began (None when idle/
        # no-target). Distinct from the trimmed history span so we know we have
        # observed for at least a full window before judging stillness.
        self._collect_start_t: Optional[float] = None
        # Learned empty-room distance (cm); None until the first still scene is
        # confirmed. When set, it drives self._far_cm.
        self._empty_room_cm: Optional[float] = None
        self._last_learn_t: float = 0.0
        # Smoothed signed velocity (cm/s) — approach negative, retreat positive.
        # Published as distance_velocity_cm_s for downstream interactivity.
        self._vel_ema: Optional[float] = None
        self._prev_t: Optional[float] = None
        self._prev_d: Optional[float] = None
        self._last_vel_published: Optional[float] = None

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="distance", daemon=True)
        self._thread.start()

    def _init_sensor(self) -> bool:
        """Open the bus, select + configure a backend. Returns success."""
        try:
            self._i2c = _open_i2c()
        except Exception as e:
            log.error("distance: I²C open failed: %s", e)
            return False
        backend = _make_backend(self._i2c)
        if backend is None:
            log.error("distance: no supported ToF sensor detected at 0x29 "
                      "(VL53_SENSOR=%s)", getattr(config, "VL53_SENSOR", "auto"))
            return False
        if not backend.configure():
            return False
        self._backend = backend
        self._far_cm = backend.far_cm
        self._far_ceiling = backend.far_cm
        log.info("distance: using %s (far=%.0fcm)", backend.name, self._far_cm)
        return True

    def _run(self) -> None:
        period = 1.0 / config.VL53_PUBLISH_HZ
        if not self.mock:
            if not self._init_sensor():
                return
        else:
            log.info("distance: mock mode")

        # Mock state
        cycle_start = time.monotonic()
        CYCLE = 25.0

        # Re-publish the (static) onset + far reach every ~2 s so a downstream
        # restart (Node server / browser reconnect) re-learns them; the Node
        # bridge dedups so the bus stays quiet between changes.
        bounds_pub_every = max(1, int(config.VL53_PUBLISH_HZ * 2))
        loop_i = 0

        while not self._stop.wait(period):
            if loop_i % bounds_pub_every == 0:
                self.ingest.publish("distance_near_cm", round(config.DISTANCE_NEAR_CM, 1))
                self.ingest.publish("distance_far_cm", round(self._far_cm, 1))
            loop_i += 1

            if self.mock:
                t = ((time.monotonic() - cycle_start) % CYCLE) / CYCLE
                if t < 0.05 or t >= 0.85:
                    raw = 200.0
                elif t < 0.35:
                    raw = 200.0 - (t - 0.05) / 0.30 * 170.0
                elif t < 0.65:
                    raw = 30.0 + 3.0 * (0.5 - abs((t - 0.50) * 4))  # tiny breathing wobble
                else:
                    raw = 30.0 + (t - 0.65) / 0.20 * 170.0
            else:
                raw = self._backend.read_raw()

            now = time.monotonic()
            self._process_sample(raw, now)

            # Quantize publication to 0.1 cm to suppress trivial JSON noise
            if self._smoothed is not None:
                v = round(self._smoothed, 1)
                if v != self._last_published:
                    self.ingest.publish("distance_cm", v)
                    self._last_published = v

    def _process_sample(self, raw: Optional[float], now: float) -> None:
        """Fold one raw read (cm, or None for no-target) into the smoothed
        trace, velocity, and empty-room learner — the single per-sample step
        shared by the live loop and the test_tof tuning view, so both behave
        identically. Updates self._smoothed and may update self._far_cm."""
        a = config.VL53_SMOOTH_ALPHA
        if raw is not None:
            # Valid read — smooth into existing trace.
            self._last_valid_t = now
            self._smoothed = raw if self._smoothed is None else (a * raw + (1 - a) * self._smoothed)
            # Velocity + empty-room learning run ONLY on real measurements —
            # never on the snapped-to-far idle value, which isn't a reading of
            # the room's geometry.
            self._track_velocity(now, self._smoothed)
            if self._collect_start_t is None:
                self._collect_start_t = now
            self._dist_hist.append((now, self._smoothed))
            cutoff = now - config.EMPTY_ROOM_STILLNESS_WINDOW_S
            while self._dist_hist and self._dist_hist[0][0] < cutoff:
                self._dist_hist.pop(0)
            if self._learn_empty_room(now):
                # Push the new far reach out immediately rather than waiting for
                # the ~2 s periodic bounds publish.
                self.ingest.publish("distance_far_cm", round(self._far_cm, 1))
        elif self._last_valid_t == 0.0 or (now - self._last_valid_t) > self.NO_TARGET_TIMEOUT_S:
            # Sustained no-target — snap to the sensor's FAR. Gradual decay would
            # leave the smoothed value stuck near the user's last close-range
            # position for ~150 ms after the hold expires, which reads as "kiosk
            # thinks I'm still here" lag.
            self._smoothed = self._far_cm
            # The snap-to-far plateau is not a measurement; drop the stillness
            # history and velocity trace so it can't be learned or counted.
            self._dist_hist.clear()
            self._collect_start_t = None
            self._prev_t = None
            self._prev_d = None
            self._vel_ema = None
        # else: brief None during a known-present target — hold value.

    def empty_room_status(self, now: float) -> dict:
        """Live snapshot of the empty-room learner for tuning UIs (test_tof).

        Mirrors the gate in _learn_empty_room without mutating anything, so the
        printed `still`/`avg_speed` match what actually drives adoption.
        """
        window = config.EMPTY_ROOM_STILLNESS_WINDOW_S
        hist = self._dist_hist
        span = (now - self._collect_start_t) if self._collect_start_t is not None else 0.0
        if len(hist) >= 2:
            lo = min(d for _, d in hist)
            hi = max(d for _, d in hist)
            pp = hi - lo
        else:
            pp = 0.0
        avg_speed = (pp / window) if window > 0 else 0.0
        full = window > 0 and span >= window
        still = full and avg_speed <= config.EMPTY_ROOM_VELOCITY_CM_S
        return {
            "smoothed": self._smoothed,
            "velocity": self._vel_ema,
            "span": span,
            "window": window,
            "pp": pp,
            "avg_speed": avg_speed,
            "window_full": full,
            "still": still,
            "empty_room": self._empty_room_cm,
            "far": self._far_cm,
            "samples": len(hist),
        }

    def _track_velocity(self, now: float, d: float) -> None:
        """Maintain a smoothed signed first derivative of distance (cm/s) and
        publish it as `distance_velocity_cm_s` for downstream interactivity
        (approach is negative, retreat positive). Stillness detection does NOT
        use this — it uses the windowed peak-to-peak in _learn_empty_room, which
        resolves sub-cm/s — so the heavy smoothing/latency here is harmless."""
        if self._prev_t is not None:
            dt = now - self._prev_t
            # Skip degenerate or post-dropout gaps: a stale prev sample across a
            # multi-frame None gap would manufacture a velocity spike.
            if 1e-4 < dt < 1.0:
                inst = (d - self._prev_d) / dt
                # EMA toward a ~0.5 s time constant so per-frame jitter averages
                # out instead of dominating.
                beta = min(1.0, dt / 0.5)
                self._vel_ema = inst if self._vel_ema is None else beta * inst + (1 - beta) * self._vel_ema
                v = round(self._vel_ema, 2)
                if v != self._last_vel_published:
                    self.ingest.publish("distance_velocity_cm_s", v)
                    self._last_vel_published = v
        self._prev_t = now
        self._prev_d = d

    def _learn_empty_room(self, now: float) -> bool:
        """If the scene has held still long enough, adopt the current reading as
        the empty-room far reach. Returns True if self._far_cm changed.

        Stillness = peak-to-peak distance excursion over a full stillness window
        staying at/below EMPTY_ROOM_VELOCITY_CM_S × window. Using the windowed
        excursion (rather than a per-sample derivative) is what makes sub-cm/s
        measurable at 50 Hz, and it rejects both steady drift and small wobble.
        """
        if not config.EMPTY_ROOM_LEARN:
            return False
        window = config.EMPTY_ROOM_STILLNESS_WINDOW_S
        hist = self._dist_hist
        # Need a continuous run of valid reads spanning at least a full window
        # before judging stillness (hist itself is trimmed TO the window, so its
        # span can't exceed it — track the run start separately).
        if (window <= 0 or len(hist) < 2 or self._collect_start_t is None
                or (now - self._collect_start_t) < window):
            return False
        lo = min(d for _, d in hist)
        hi = max(d for _, d in hist)
        avg_speed = (hi - lo) / window  # cm/s; the "velocity" inactivity gauge
        if avg_speed > config.EMPTY_ROOM_VELOCITY_CM_S:
            return False
        # Throttle re-adoption while a still scene persists (update periodically,
        # not every frame).
        if (now - self._last_learn_t) < config.EMPTY_ROOM_RELEARN_S:
            return False
        # Robust central distance of the still window.
        vals = sorted(d for _, d in hist)
        candidate = vals[len(vals) // 2]
        # A reading nearer than the plausible floor is a motionless subject, not
        # the room — never let it become or lower the baseline.
        if candidate < config.EMPTY_ROOM_MIN_CM:
            return False

        self._last_learn_t = now
        if self._empty_room_cm is None:
            est = candidate                      # first confirmation: adopt as-is
        elif candidate >= self._empty_room_cm:
            est = candidate                      # farther/clearer: trust immediately
        else:
            # Closer: a genuine layout change OR a still visitor. Ease in slowly
            # so a transient still person barely moves it and it recovers.
            a = config.EMPTY_ROOM_DOWN_ALPHA
            est = self._empty_room_cm + a * (candidate - self._empty_room_cm)
        # Clamp to a sane band: never below the floor, never past the sensor's
        # reliable reach.
        est = max(config.EMPTY_ROOM_MIN_CM, min(est, self._far_ceiling))
        self._empty_room_cm = est

        if round(est, 1) != round(self._far_cm, 1):
            self._far_cm = est
            log.info("distance: empty-room far updated -> %.1f cm "
                     "(candidate=%.1f, pp=%.2f cm over %.0fs)",
                     est, candidate, hi - lo, window)
            return True
        return False

    def stop(self) -> None:
        self._stop.set()
        if self._backend is not None:
            self._backend.stop()
        if self._thread is not None:
            self._thread.join(timeout=1.0)
