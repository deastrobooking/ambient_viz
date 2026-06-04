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
const envNum = (k, d) => { const v = parseFloat(process.env[k]); return Number.isFinite(v) ? v : d; };

// --- Bell-on-entry ----------------------------------------------------------
// Ring the firmware's FM bell (a MIDI note-on; FmStab strikes it on top of the
// mix, just before the limiter) the moment someone enters the space.
//
// "Empty room" = the learned far reach (distance_far_cm, the empty-room
// distance the Python sidecar learns). The trigger is edge-detected by a small
// state machine so it fires exactly once per arrival:
//   - ARM only after the room has read empty for BELL_REARM_EMPTY_S, so a
//     fresh trigger always follows a genuine "the room went quiet" stretch.
//   - FIRE when an armed room is penetrated by BELL_ENTER_FRACTION of its depth
//     AND the visitor is actively moving inward (distance_velocity_cm_s, which
//     is negative on approach). The depth gate rejects someone hovering at the
//     edge; the velocity gate rejects a slow furniture shuffle or a drifting
//     empty-room estimate.
//   - then DISARM until the room returns to empty for the full re-arm window.
const BELL_NOTE = clamp(Math.round(envNum('BELL_NOTE', 81)), 0, 127); // A5 ~880 Hz
const BELL_VELOCITY = clamp(Math.round(envNum('BELL_VELOCITY', 100)), 1, 127);
// 1-in-N entries strike the harsher "industrial" FmStab patch instead of the
// bell. Same trigger + mix path; only the timbre differs, selected by MIDI
// channel (ch1 = industrial) so the firmware swaps the patch for that strike.
const BELL_INDUSTRIAL_PROB = clamp(envNum('BELL_INDUSTRIAL_PROB', 0.1), 0, 1);
const BELL_INDUSTRIAL_NOTE = clamp(Math.round(envNum('BELL_INDUSTRIAL_NOTE', BELL_NOTE)), 0, 127);
const BELL_ENTER_FRACTION = envNum('BELL_ENTER_FRACTION', 0.15); // come in 15% of the depth
const BELL_EMPTY_FRACTION = envNum('BELL_EMPTY_FRACTION', 0.08); // within 8% of far = "empty"
const BELL_REARM_EMPTY_S = envNum('BELL_REARM_EMPTY_S', 10); // hold empty this long to re-arm
const BELL_APPROACH_CM_S = envNum('BELL_APPROACH_CM_S', 2.0); // min inward speed to count as entering
const BELL_TICK_MS = 500; // re-evaluate arming even when distance_cm is static

let port = null;
let reopenTimer = null;
let bellTimer = null;
const lastCc = {}; // cc# -> last value sent (dedupe)
const lastWriteAt = {}; // cc# -> last write time (throttle)

// Bell-on-entry state.
let lastDistanceCm = null; // most recent distance_cm
let lastVelocityCmS = 0; // most recent distance_velocity_cm_s (approach negative)
let bellArmed = false; // ready to ring on the next entry
let emptySinceMs = null; // when the room first read empty in the current empty run

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

function writeNoteOn(note, velocity, channel = 0) {
  if (!port || !port.isOpen) return;
  // note-on; velocity 0 would read as note-off, so floor at 1. The channel
  // selects the FM timbre on the firmware's shared bank (0 = bell, 1 = industrial).
  const n = clamp(Math.round(note), 0, 127);
  const v = clamp(Math.round(velocity), 1, 127);
  port.write(Buffer.from([0x90 | (clamp(Math.round(channel), 0, 15)), n, v]));
}

// Edge-detect "someone entered the space" against the learned empty-room reach
// and ring the bell once. Called both on distance_cm updates (snappy trigger)
// and on a slow timer (so arming still progresses when the empty room is so
// still that distance_cm stops changing).
function updateBellTrigger(nowMs) {
  if (lastDistanceCm === null || !(farCm > 0)) return;
  const enterThresh = farCm * (1 - BELL_ENTER_FRACTION); // cross BELOW to trigger
  const emptyThresh = farCm * (1 - BELL_EMPTY_FRACTION); // stay ABOVE to count as empty

  if (lastDistanceCm >= emptyThresh) {
    // Room reads empty — run the re-arm timer.
    if (emptySinceMs === null) emptySinceMs = nowMs;
    if (!bellArmed && nowMs - emptySinceMs >= BELL_REARM_EMPTY_S * 1000) {
      bellArmed = true;
    }
    return;
  }

  // Someone/something is nearer than empty — the empty run is broken.
  emptySinceMs = null;

  // Fire once: armed, penetrated the entry depth, and actively moving inward.
  const inwardSpeed = -lastVelocityCmS; // approach is negative
  if (bellArmed && lastDistanceCm <= enterThresh && inwardSpeed >= BELL_APPROACH_CM_S) {
    const industrial = Math.random() < BELL_INDUSTRIAL_PROB;
    if (industrial) {
      writeNoteOn(BELL_INDUSTRIAL_NOTE, BELL_VELOCITY, 1);
    } else {
      writeNoteOn(BELL_NOTE, BELL_VELOCITY, 0);
    }
    bellArmed = false; // locked until the room returns to empty for the re-arm window
    console.log(
      `daisy-position: ${industrial ? 'industrial' : 'bell'} (entry at `
      + `${lastDistanceCm.toFixed(0)}cm, ${inwardSpeed.toFixed(1)}cm/s in, far=${farCm.toFixed(0)}cm)`,
    );
  }
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
    lastDistanceCm = value;
    updateBellTrigger(Date.now());
  } else if (name === 'distance_velocity_cm_s') {
    lastVelocityCmS = value;
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

  // Tunnel distance_cm / freeze back to the Daisy as CC; ring the bell on entry.
  if (bus) bus.on('change', (e) => onChange(e.name, e.value));

  // Keep the re-arm timer advancing even while a dead-still empty room emits no
  // distance_cm changes (it's deduped on-change upstream).
  bellTimer = setInterval(() => updateBellTrigger(Date.now()), BELL_TICK_MS);
  if (typeof bellTimer.unref === 'function') bellTimer.unref();

  open();
};

module.exports.stop = () => {
  if (reopenTimer) { clearTimeout(reopenTimer); reopenTimer = null; }
  if (bellTimer) { clearInterval(bellTimer); bellTimer = null; }
  if (port) {
    try { port.close(); } catch { /* */ }
    port = null;
  }
};
