// Owns the Daisy's USB-CDC serial port, full-duplex (PLAN_USB_COMPOSITE):
//
//  READ  (Phase D): the Daisy emits `POS <sec>\n` ~20x/s and `RESET <sec>\n`
//    once per loop wrap; we republish both as the `song_position` SSE topic so
//    the visualizer can sync its lanes (it rebases on every report, so the
//    backward jump a RESET produces hard-snaps — no separate signal needed).
//
//  WRITE (Phase E): we tunnel sensor/freeze control to the Daisy as raw 3-byte
//    MIDI CC frames `[0xB0, cc, value]` (the firmware frames + decodes them):
//      - `distance_cm`  -> CC 23 (tape failure): near (the onset) = present =
//        pristine, far (≥ the sensor's reach) = absent = tape eaten.
//      - `distance_near_cm` / `distance_far_cm` -> set the onset + far reach of
//        that curve (config-driven / sensor-mode dependent); not forwarded to
//        the Daisy, they only shape the distance_cm mapping above.
//      - `freeze` (0..1, posted by the browser to /ingest) -> CC 24.
//    On-change + rate-capped so the bulk-OUT pipe + param smoothers don't flood.
//
// This is the SINGLE owner of the fd. Do NOT open a second writer elsewhere — a
// tty has one clean reader/writer; concurrent writers corrupt MIDI frames.

const { SerialPort, ReadlineParser } = require('serialport');

// macOS has no stable device name (it's /dev/cu.usbmodemXXXX); the Pi is
// /dev/ttyACM0. Require an explicit DAISY_SERIAL on macOS.
const DEFAULT_PATH = process.platform === 'darwin' ? null : '/dev/ttyACM0';
const LINE_RE = /^(?:POS|RESET)\s+([0-9]+(?:\.[0-9]+)?)/;
const REOPEN_MS = 1000;

// CC map — must match dsp::install_kiosk_bindings on the firmware.
const CC_TAPE_FAILURE = 23;
const CC_FREEZE = 24;
// Distance -> failure curve (cm). Mirror of the visualizer's presence shaping:
// at/within the onset the tape sits at its subtle default (failure 0); past it
// the deck falls apart with distance, fully destroyed at/beyond the far reach.
// NEITHER end is fixed — both track values published by the Python sidecar so
// they're tunable on install day without rebuilding: `distance_near_cm` is the
// onset (one knob in config.py, shared with the visualizer), `distance_far_cm`
// the sensor's mode-derived reach (short ~130 cm / long ~400 cm). We default
// until the first values arrive.
const NEAR_DEFAULT_CM = 75;
const FAR_DEFAULT_CM = 130;
let nearCm = NEAR_DEFAULT_CM;
let farCm = FAR_DEFAULT_CM;
const MIN_WRITE_MS = 33; // cap each CC to ~30 Hz (complication #13)

const clamp = (x, a, b) => Math.min(b, Math.max(a, x));

let port = null;
let reopenTimer = null;
const lastCc = {}; // cc# -> last value sent (dedupe)
const lastWriteAt = {}; // cc# -> last write time (throttle)

function writeCc(cc, value) {
  const v = clamp(Math.round(value), 0, 127);
  if (lastCc[cc] === v) return; // on-change only
  const now = Date.now();
  if (now - (lastWriteAt[cc] || 0) < MIN_WRITE_MS) return; // rate cap
  if (!port || !port.isOpen) return;
  port.write(Buffer.from([0xb0, cc, v]));
  lastCc[cc] = v;
  lastWriteAt[cc] = now;
}

function onChange(name, value) {
  if (typeof value !== 'number') return;
  if (name === 'distance_near_cm') {
    // Effect onset, shared with the visualizer; keep it below the far reach.
    if (value >= 0 && value < farCm) nearCm = value;
  } else if (name === 'distance_far_cm') {
    // Sensor's mode-derived reach; the far end of the failure ramp tracks it.
    if (value > nearCm) farCm = value;
  } else if (name === 'distance_cm') {
    const span = farCm - nearCm;
    const failure = span > 0 ? clamp((value - nearCm) / span, 0, 1) : (value >= farCm ? 1 : 0);
    writeCc(CC_TAPE_FAILURE, failure * 127);
  } else if (name === 'freeze') {
    writeCc(CC_FREEZE, clamp(value, 0, 1) * 127);
  }
}

module.exports = ({ publish, bus }) => {
  const portPath = process.env.DAISY_SERIAL || DEFAULT_PATH;
  if (!portPath) {
    console.warn(
      'daisy-position: no port — set DAISY_SERIAL=/dev/cu.usbmodemXXXX (macOS has no fixed name)',
    );
    return;
  }

  const scheduleReopen = () => {
    if (reopenTimer) return;
    if (port) {
      try { port.close(); } catch { /* already gone */ }
      port = null;
    }
    reopenTimer = setTimeout(() => { reopenTimer = null; open(); }, REOPEN_MS);
  };

  const open = () => {
    port = new SerialPort({ path: portPath, baudRate: 115200 }, (err) => {
      if (err) { scheduleReopen(); return; }
      // Assert DTR: the firmware only emits POS once the host opens the port
      // (its `wait_connection` waits on the DTR line state).
      try { port.set({ dtr: true, rts: true }, () => {}); } catch { /* */ }
      console.log(`daisy-position: reading + writing ${portPath}`);
    });
    port.pipe(new ReadlineParser({ delimiter: '\n' })).on('data', (line) => {
      const m = LINE_RE.exec(String(line).trim());
      if (m) publish('song_position', parseFloat(m[1]));
    });
    // Hot-plug resilience: reopen on any error/close (unplug, Daisy reboot).
    port.on('error', scheduleReopen);
    port.on('close', scheduleReopen);
  };

  // Tunnel distance_cm / freeze back to the Daisy as CC.
  if (bus) bus.on('change', (e) => onChange(e.name, e.value));

  open();
};

module.exports.stop = () => {
  if (reopenTimer) { clearTimeout(reopenTimer); reopenTimer = null; }
  if (port) {
    try { port.close(); } catch { /* */ }
    port = null;
  }
};
