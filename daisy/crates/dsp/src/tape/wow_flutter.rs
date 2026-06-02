//! Wow + flutter — pitch modulation via a fractional delay line.
//!
//! Mirrors CHOWTape's `WowFlutterProcessor` topology:
//! - **One shared modulation signal** for both stereo channels (real tape has
//!   a single capstan/pinch roller moving both heads — the audio character is
//!   correlated pitch wobble across L/R).
//! - **Independent delay buffers** per channel (they hold different audio).
//! - **Wow**: one slow cosine + a slow random-walk drift, summed.
//! - **Flutter**: three summed cosines at slightly offset rates so the motion
//!   doesn't feel periodic — same trick CHOWTape uses.
//!
//! Read position is `base_delay - modulation`, where positive modulation
//! pulls the read earlier (faster playback → pitch up). The base delay
//! exists only to give the read head headroom to swing in both directions.

use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::TAU;
/// Fast cos (~0.2% error) for the wow/flutter LFOs. libm `cosf` measured ~1250
/// cycles on the Cortex-M7 (argument range reduction) and dominated the tape's
/// CPU cost; this parabolic approximation (Nick's, refined) is ~30x cheaper and
/// far more accurate than an LFO needs.
#[inline]
fn fast_cos(x: f32) -> f32 {
    use core::f32::consts::{PI, TAU};
    // Reduce to [-PI, PI) — args here are non-negative LFO phases * TAU.
    let n = (x * (1.0 / TAU) + 0.5) as i32;
    let a = x - TAU * n as f32;
    // cos(a) = sin(a + PI/2); wrap the shifted argument back into [-PI, PI).
    let mut b = a + PI * 0.5;
    if b > PI {
        b -= TAU;
    }
    let abs_b = if b < 0.0 { -b } else { b };
    let y = (4.0 / PI) * b - (4.0 / (PI * PI)) * b * abs_b;
    let abs_y = if y < 0.0 { -y } else { y };
    0.225 * (y * abs_y - y) + y
}

/// Simple ring-buffer fractional delay with linear interpolation.
struct DelayLine {
    buf: Vec<f32>,
    write_idx: usize,
}

impl DelayLine {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0.0; size],
            write_idx: 0,
        }
    }

    /// Push `input`, return the sample at `delay_samples` ago (fractional).
    fn process(&mut self, input: f32, delay_samples: f32) -> f32 {
        let len = self.buf.len();
        self.buf[self.write_idx] = input;

        // Wrap into [0, len). delay_samples is clamped by the caller so this
        // single adjustment is enough.
        let mut read_pos = self.write_idx as f32 - delay_samples;
        if read_pos < 0.0 {
            read_pos += len as f32;
        }
        let read_int = (read_pos as usize) % len;
        let next_int = (read_int + 1) % len;
        let frac = read_pos - (read_pos as usize) as f32;
        let out = self.buf[read_int] + (self.buf[next_int] - self.buf[read_int]) * frac;

        self.write_idx = (self.write_idx + 1) % len;
        out
    }
}

pub struct WowFlutter {
    sample_rate: f32,
    inv_sr: f32,
    enabled: bool,

    /// Wow oscillator phase, [0, 1).
    wow_phase: f32,
    /// Wow random-walk drift, clamped to [-0.5, 0.5].
    wow_drift: f32,

    /// Flutter oscillator phases, [0, 1) each.
    flutter_phases: [f32; 3],
    /// Initial phase offsets so the three flutter cosines don't start in sync.
    flutter_offsets: [f32; 3],

    /// Modulation rates (Hz).
    wow_rate: f32,
    flutter_rates: [f32; 3],

    /// Modulation depths (samples). `set_wow_depth_ms` / `set_flutter_depth_ms`
    /// convert from ms.
    wow_depth_samples: f32,
    flutter_depth_samples: f32,
    /// Per-sample increment of the wow drift random walk.
    wow_drift_rate: f32,

    /// Base delay (samples). The read sits here when modulation is zero;
    /// modulation swings it ±max_depth.
    base_delay_samples: f32,

    delay_l: DelayLine,
    delay_r: DelayLine,

    /// xorshift32 state for wow drift.
    rng_state: u32,
}

impl WowFlutter {
    pub fn new(sample_rate: f32) -> Self {
        // 50 ms buffer per channel — plenty for any realistic modulation.
        let buf_size = (0.050 * sample_rate) as usize;
        // 12 ms base delay: enough headroom for ±10 ms wow + ±1 ms flutter.
        // Adds 12 ms of latency to the master bus, which is imperceptible
        // for a non-realtime-monitoring use case.
        let base_delay = 0.012 * sample_rate;

        Self {
            sample_rate,
            inv_sr: 1.0 / sample_rate,
            enabled: true,

            wow_phase: 0.0,
            wow_drift: 0.0,
            flutter_phases: [0.0; 3],
            flutter_offsets: [0.0, 0.31, 0.68],

            wow_rate: 0.6,
            flutter_rates: [6.0, 7.3, 9.0],

            wow_depth_samples: 0.005 * sample_rate, // 5 ms — audible by default
            flutter_depth_samples: 0.0005 * sample_rate, // 0.5 ms (per cosine; /3 in process)
            wow_drift_rate: 0.0001,

            base_delay_samples: base_delay,
            delay_l: DelayLine::new(buf_size),
            delay_r: DelayLine::new(buf_size),
            rng_state: 0x1234_5678,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Wow depth in milliseconds (peak deviation). Typical 1-10 ms.
    pub fn set_wow_depth_ms(&mut self, ms: f32) {
        self.wow_depth_samples = ms.max(0.0) * 0.001 * self.sample_rate;
    }

    /// Flutter depth in milliseconds (peak deviation per cosine, summed/3).
    /// Typical 0.1-1.0 ms.
    pub fn set_flutter_depth_ms(&mut self, ms: f32) {
        self.flutter_depth_samples = ms.max(0.0) * 0.001 * self.sample_rate;
    }

    /// Wow rate in Hz. Typical 0.5-2 Hz.
    pub fn set_wow_rate_hz(&mut self, hz: f32) {
        self.wow_rate = hz.max(0.0);
    }

    /// xorshift32 → bipolar f32 in [-1, 1).
    #[inline]
    fn rand_bipolar(&mut self) -> f32 {
        let mut x = self.rng_state.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Process one stereo frame in-place. Same modulation signal applied to
    /// both channels (correlated, like a single capstan).
    pub fn process_sample(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.enabled {
            return (l, r);
        }

        // --- Wow ---
        self.wow_phase += self.wow_rate * self.inv_sr;
        if self.wow_phase >= 1.0 {
            self.wow_phase -= 1.0;
        }
        self.wow_drift += self.rand_bipolar() * self.wow_drift_rate;
        if self.wow_drift > 0.5 {
            self.wow_drift = 0.5;
        } else if self.wow_drift < -0.5 {
            self.wow_drift = -0.5;
        }
        let wow = fast_cos(self.wow_phase * TAU) * self.wow_depth_samples
            + self.wow_drift * self.wow_depth_samples;

        // --- Flutter (three summed cosines at offset rates/phases) ---
        let mut flutter = 0.0;
        for i in 0..3 {
            self.flutter_phases[i] += self.flutter_rates[i] * self.inv_sr;
            if self.flutter_phases[i] >= 1.0 {
                self.flutter_phases[i] -= 1.0;
            }
            flutter += fast_cos((self.flutter_phases[i] + self.flutter_offsets[i]) * TAU);
        }
        flutter *= self.flutter_depth_samples * (1.0 / 3.0);

        // Clamp the total modulation so we never exceed the base-delay headroom.
        // Underrun would cause the read head to lap the write head → garbage.
        let total_mod = (wow + flutter).clamp(
            -(self.base_delay_samples - 2.0),
            self.base_delay_samples - 2.0,
        );
        let delay = self.base_delay_samples + total_mod;

        (self.delay_l.process(l, delay), self.delay_r.process(r, delay))
    }
}
