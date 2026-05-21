// Fake input source: emits the same wire format the Python sensor sidecar
// produces in production, so the browser-side SSE plumbing can be
// validated on a machine without GPIO.
//
// Real production path: Python drivers POST to /ingest. This in-process
// mock skips the HTTP hop and publishes directly to the bus.
//
// Event names match hardware-handoff.md's vocabulary:
//   motion           — bool, AM312 PIR state (latched while motion is held)
//   distance_cm      — number 25..120, VL53L1X distance with smoothing
//   breath_detected  — timestamp of the most recent breath puff (bumps
//                      monotonically; visualizer compares to its clock)
//   touch_mask       — 12-bit int, MPR121 channel touch state
//
// Patterns mimic realistic-ish kiosk behavior: occasional person walking
// up (motion + distance ramping down), occasional breath event, drifting
// touches.

const timers = [];

module.exports = ({ publish }) => {
  // Person-walks-up cycle: every ~25 s, simulate someone approaching,
  // standing, then leaving.
  let cycleT = 0;
  const CYCLE_MS = 25000;
  timers.push(setInterval(() => {
    cycleT = (cycleT + 100) % CYCLE_MS;
    const t = cycleT / CYCLE_MS; // 0..1 across the cycle
    if (t < 0.05) {
      publish('motion', false);
      publish('distance_cm', 200);
    } else if (t < 0.35) {
      // Approaching: distance ramps 200 → 30
      publish('motion', true);
      const d = 200 - (t - 0.05) / 0.30 * 170;
      publish('distance_cm', +d.toFixed(1));
    } else if (t < 0.65) {
      // Standing close, slight breathing wobble
      publish('motion', true);
      publish('distance_cm', +(30 + 3 * Math.sin(cycleT / 600)).toFixed(1));
    } else if (t < 0.85) {
      // Leaving: 30 → 200
      publish('motion', true);
      const d = 30 + (t - 0.65) / 0.20 * 170;
      publish('distance_cm', +d.toFixed(1));
    } else {
      publish('motion', false);
      publish('distance_cm', 200);
    }
  }, 100));

  // Breath events: fire near the middle of the standing-close phase
  // (~12.5 s into each cycle). Slight jitter so not perfectly periodic.
  timers.push(setInterval(() => {
    if (Math.random() < 0.6) publish('breath_detected', Date.now());
  }, 11000 + Math.random() * 4000));

  // Cap touch: drifting bitmask, occasional multi-pad press
  let mask = 0;
  timers.push(setInterval(() => {
    if (Math.random() < 0.3) {
      // press: set a random pad
      mask |= 1 << Math.floor(Math.random() * 12);
    } else {
      // release: clear a random set bit
      const setBits = [];
      for (let i = 0; i < 12; i++) if (mask & (1 << i)) setBits.push(i);
      if (setBits.length) mask &= ~(1 << setBits[Math.floor(Math.random() * setBits.length)]);
    }
    publish('touch_mask', mask);
  }, 1500));
};

module.exports.stop = () => {
  for (const t of timers) clearInterval(t);
  timers.length = 0;
};
