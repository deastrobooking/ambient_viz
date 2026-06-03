"""VL53L1X time-of-flight distance sensor over I²C.

Polls at ~50 Hz, smooths with an exponential moving average, publishes
`distance_cm`. Invalid reads (no target in cone) decay smoothed value
toward `far` rather than freezing.
"""

import logging
import threading
import time
from typing import Optional

from .. import config

log = logging.getLogger(__name__)


class DistanceDriver:
    # Seconds with no valid read before we snap to VL53_FAR_CM. Long
    # enough to ride out the typical multi-frame dropouts on a real
    # target (a moving hand drops 1-3 frames at a time), short enough
    # that walking out of the cone shows up as "idle" within a beat
    # rather than holding the user's last close-range position for
    # multiple seconds.
    NO_TARGET_TIMEOUT_S = 0.6

    # VL53L1X result register holding the per-measurement ambient IR count
    # rate (ST ULD name: VL53L1_RESULT__AMBIENT_COUNT_RATE_MCPS_SD0). The
    # Adafruit CircuitPython lib doesn't surface this, so we read it raw.
    # Only valid after a completed measurement (data_ready).
    _REG_AMBIENT_RATE = 0x0090

    def __init__(self, ingest, mock: bool = False):
        self.ingest = ingest
        self.mock = mock
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._sensor = None
        self._i2c = None
        self._smoothed: Optional[float] = None
        self._last_published: Optional[float] = None
        # Sensor's far reach in cm — the no-target snap value and the saturation
        # end of the downstream effect mappings. Mode dependent; resolved once
        # the distance mode is chosen in _init_sensor (short default until then).
        self._far_cm: float = config.VL53_FAR_CM

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

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="distance", daemon=True)
        self._thread.start()

    def _init_sensor(self) -> bool:
        try:
            import board
            import busio
            import adafruit_vl53l1x
            self._i2c = busio.I2C(board.SCL, board.SDA)
            self._sensor = adafruit_vl53l1x.VL53L1X(self._i2c, address=config.VL53L1X_ADDR)
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
            self._far_cm = self._far_for_mode(mode)
            log.info("distance: VL53L1X ready (mode=%d, budget=%dms, far=%.0fcm)",
                     mode, self._budget_for_mode(mode), self._far_cm)
            return True
        except Exception as e:
            log.error("distance: VL53L1X init failed: %s", e)
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

    def _read_raw(self) -> Optional[float]:
        """Returns distance in cm, or None for no-target / invalid."""
        try:
            if not self._sensor.data_ready:
                return None
            d = self._sensor.distance  # cm; None if no valid target
            self._sensor.clear_interrupt()
            return float(d) if d is not None else None
        except Exception as e:
            log.debug("distance: read error: %s", e)
            return None

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

        # Wall-clock of the last valid (non-None) read. Stays 0.0 until the
        # first one arrives, so initial published value is FAR (idle).
        last_valid_t = 0.0

        # Re-publish the (static) far reach every ~2 s so a downstream restart
        # (Node server / browser reconnect) re-learns it; the Node bridge dedups
        # so the bus stays quiet between changes.
        far_pub_every = max(1, int(config.VL53_PUBLISH_HZ * 2))
        loop_i = 0

        while not self._stop.wait(period):
            if loop_i % far_pub_every == 0:
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
                raw = self._read_raw()

            now = time.monotonic()
            a = config.VL53_SMOOTH_ALPHA
            if raw is not None:
                # Valid read — smooth into existing trace.
                last_valid_t = now
                self._smoothed = raw if self._smoothed is None else (a * raw + (1 - a) * self._smoothed)
            elif last_valid_t == 0.0 or (now - last_valid_t) > self.NO_TARGET_TIMEOUT_S:
                # Sustained no-target — snap to the mode-derived FAR. Gradual
                # decay would leave the smoothed value stuck near the user's last
                # close-range position for ~150 ms after the hold expires, which
                # reads as "kiosk thinks I'm still here" lag.
                self._smoothed = self._far_cm
            # else: brief None during a known-present target — hold value.

            # Quantize publication to 0.1 cm to suppress trivial JSON noise
            if self._smoothed is not None:
                v = round(self._smoothed, 1)
                if v != self._last_published:
                    self.ingest.publish("distance_cm", v)
                    self._last_published = v

    def stop(self) -> None:
        self._stop.set()
        if self._sensor is not None:
            try:
                self._sensor.stop_ranging()
            except Exception:
                pass
        if self._thread is not None:
            self._thread.join(timeout=1.0)
