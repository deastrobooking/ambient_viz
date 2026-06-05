"""AM312 PIR motion sensor(s).

Two AM312 units are fanned outward from the wall for wide room coverage and
combined with a logical OR into a single `motion` channel: motion in *either*
cone means "someone is in the room" (see hardware-handoff.md). The driver
publishes `motion` (bool) only when the OR'd state changes, and suppresses all
events for the first 60s post-process-start so the AM312 settling phase doesn't
false-trigger.

The sensor count comes from config.PIR_PINS (env-overridable). A sensor that
fails to initialise is skipped with a warning rather than killing the driver,
so a partial install (one AM312 wired, or a flaky one) still publishes whatever
coverage it has. With zero usable sensors the driver stays inert — the `motion`
channel is simply never published, and any downstream presence logic gated on
it falls back to its non-motion path.
"""

import logging
import threading
import time

from .. import config

log = logging.getLogger(__name__)


class PirDriver:
    def __init__(self, ingest, pins=None, mock: bool = False):
        self.ingest = ingest
        self.pins = list(config.PIR_PINS if pins is None else pins)
        self.mock = mock
        self._start_t = 0.0
        self._sensors = []
        self._active_pins = []  # BCM pins that initialised OK
        self._or_state = False  # last published OR across all sensors
        self._mock_thread = None
        self._stop = threading.Event()

    def _suppressed(self) -> bool:
        return (time.monotonic() - self._start_t) < config.PIR_BOOT_SUPPRESS_S

    def _publish_or(self) -> None:
        """Recompute the OR across all sensors; publish only on change."""
        if self._suppressed():
            return
        state = any(s.motion_detected for s in self._sensors)
        if state != self._or_state:
            self._or_state = state
            self.ingest.publish("motion", state)

    def _emit_mock(self, value: bool) -> None:
        if self._suppressed():
            return
        if value != self._or_state:
            self._or_state = value
            self.ingest.publish("motion", value)

    def start(self) -> None:
        self._start_t = time.monotonic()
        if self.mock:
            self._mock_thread = threading.Thread(target=self._mock_loop, name="pir-mock", daemon=True)
            self._mock_thread.start()
            log.info("pir: mock mode")
            return
        if not self.pins:
            log.warning("pir: no PIR pins configured (PIR_PINS empty) — motion channel disabled")
            return
        # Lazy import: gpiozero only installs on Pi
        from gpiozero import MotionSensor
        for pin in self.pins:
            try:
                sensor = MotionSensor(pin)
            except Exception as e:
                # A miswired / absent AM312 must not take down the others. Skip it.
                log.warning("pir: AM312 on BCM%d failed to init (%s) — skipping", pin, e)
                continue
            # Each transition on any sensor re-evaluates the OR.
            sensor.when_motion = lambda: self._publish_or()
            sensor.when_no_motion = lambda: self._publish_or()
            self._sensors.append(sensor)
            self._active_pins.append(pin)
        if not self._sensors:
            log.warning("pir: no AM312 sensors initialised — motion channel disabled")
            return
        log.info("pir: %d AM312 sensor(s) on BCM%s, OR'd (suppressing %.0fs post-boot)",
                 len(self._sensors), self._active_pins, config.PIR_BOOT_SUPPRESS_S)
        # Seed initial state (might be high if someone happens to be there at boot).
        self._publish_or()

    def _mock_loop(self) -> None:
        # Toggle every ~10s with some jitter; track expected state.
        state = False
        while not self._stop.wait(8 + (hash(time.monotonic()) & 7)):
            state = not state
            self._emit_mock(state)

    def stop(self) -> None:
        self._stop.set()
        for sensor in self._sensors:
            try:
                sensor.close()
            except Exception:
                pass
        self._sensors = []
        self._active_pins = []
        if self._mock_thread is not None:
            self._mock_thread.join(timeout=0.5)
