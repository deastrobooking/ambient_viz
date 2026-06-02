// Tails the Daisy's USB-CDC serial port and republishes song position as the
// `song_position` SSE topic, so the visualizer can drive its automation lanes
// off the position the Daisy is actually playing (same USB SOF clock owns the
// audio and this stream — no cross-cable drift).
//
// The Daisy emits (PLAN_USB_COMPOSITE Phase C):
//   POS <seconds>\n      ~20x/second
//   RESET <seconds>\n    once per loop wrap (value ~0)
// Both carry a position in seconds. We publish both the same way — the
// visualizer hard-snaps on the backward jump a RESET produces (it rebases its
// interpolation on every report), so no separate reset signal is needed.
//
// This process OWNS the serial fd. Phase E adds the write direction (sensor CC
// frames) on this same port — do not open a second owner elsewhere (a tty has
// one clean reader/writer; concurrent writers corrupt frames).

const { SerialPort, ReadlineParser } = require('serialport');

// macOS has no stable device name (it's /dev/cu.usbmodemXXXX); the Pi is
// /dev/ttyACM0. Require an explicit DAISY_SERIAL on macOS.
const DEFAULT_PATH = process.platform === 'darwin' ? null : '/dev/ttyACM0';
const LINE_RE = /^(?:POS|RESET)\s+([0-9]+(?:\.[0-9]+)?)/;
const REOPEN_MS = 1000;

let port = null;
let reopenTimer = null;

module.exports = ({ publish }) => {
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
      console.log(`daisy-position: reading ${portPath}`);
    });
    port.pipe(new ReadlineParser({ delimiter: '\n' })).on('data', (line) => {
      const m = LINE_RE.exec(String(line).trim());
      if (m) publish('song_position', parseFloat(m[1]));
    });
    // Hot-plug resilience: reopen on any error/close (unplug, Daisy reboot).
    port.on('error', scheduleReopen);
    port.on('close', scheduleReopen);
  };

  open();
};

module.exports.stop = () => {
  if (reopenTimer) { clearTimeout(reopenTimer); reopenTimer = null; }
  if (port) {
    try { port.close(); } catch { /* */ }
    port = null;
  }
};
