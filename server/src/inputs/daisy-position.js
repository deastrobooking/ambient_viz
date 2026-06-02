// Owns the Daisy's USB-CDC serial port, full-duplex (PLAN_USB_COMPOSITE):
//
//  READ  (Phase D): the Daisy emits `POS <sec>\n` ~20x/s and `RESET <sec>\n`
//    once per loop wrap; we republish both as the `song_position` SSE topic so
//    the visualizer can sync its lanes (it rebases on every report, so the
//    backward jump a RESET produces hard-snaps — no separate signal needed).
//
//  WRITE (Phase E): we tunnel sensor/freeze control to the Daisy as raw 3-byte
//    MIDI CC frames `[0xB0, cc, value]` (the firmware frames + decodes them):
//      - `distance_cm`  -> CC 23 (tape failure): near = present = pristine,
//        far = absent = tape eaten.
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
// near (present) resolves the tape; far (absent) lets it fall apart.
const NEAR_CM = 25;
const FAR_CM = 100;
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
  if (name === 'distance_cm') {
    const failure = clamp((value - NEAR_CM) / (FAR_CM - NEAR_CM), 0, 1);
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
