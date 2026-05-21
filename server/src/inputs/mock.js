// Fake input source for development on machines without GPIO.
// Emits values shaped like what the real drivers will publish, so the
// browser-side client and any future visualizer mappings can be wired
// before any hardware exists.

const timers = [];

module.exports = ({ publish }) => {
  let p1 = false, p2 = false;
  timers.push(setInterval(() => { p1 = !p1; publish('tog.power', p1); }, 3700));
  timers.push(setInterval(() => { p2 = !p2; publish('tog.invert', p2); }, 7100));

  timers.push(setInterval(() => {
    publish('pir.motion', 1);
    setTimeout(() => publish('pir.motion', 0), 600);
  }, 5000));

  const KEYS = ['1','2','3','A','4','5','6','B','7','8','9','C','*','0','#','D'];
  timers.push(setInterval(() => {
    publish('key.last', KEYS[Math.floor(Math.random() * KEYS.length)]);
  }, 4000));

  const t0 = Date.now();
  timers.push(setInterval(() => {
    const t = (Date.now() - t0) / 1000;
    publish('humidity', +(0.5 + 0.4 * Math.sin(t * 0.12)).toFixed(3));
  }, 500));

  let mask = 0;
  timers.push(setInterval(() => {
    mask ^= 1 << Math.floor(Math.random() * 12);
    publish('touch.mask', mask);
  }, 1200));
};

module.exports.stop = () => {
  for (const t of timers) clearInterval(t);
  timers.length = 0;
};
