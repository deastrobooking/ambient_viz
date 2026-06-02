//! Master peak limiter — linked stereo, smoothed gain, feed-forward (no
//! lookahead). Transparent while peaks stay below [`THRESHOLD`], so the dry
//! master passes unchanged; it engages only when the summed signal (master +
//! the parallel freeze ghost) would exceed the ceiling, brick-walling it so the
//! ghost can't clip or push the level past the original.
//!
//! Linked: one gain drives both channels (detected from the max of the two), so
//! limiting never shifts the stereo image.

use libm::expf;

/// Ceiling (linear). Just under full scale — only catches near-clip peaks.
const THRESHOLD: f32 = 0.99;
/// Gain-reduction attack / recovery time constants, seconds.
const ATTACK_S: f32 = 0.001;
const RELEASE_S: f32 = 0.1;

pub struct Limiter {
    gain: f32,
    atk: f32,
    rel: f32,
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            gain: 1.0,
            atk: 1.0 - expf(-1.0 / (ATTACK_S * sample_rate)),
            rel: 1.0 - expf(-1.0 / (RELEASE_S * sample_rate)),
        }
    }

    /// Limit in place (stereo interleaved).
    pub fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            let al = if frame[0] < 0.0 { -frame[0] } else { frame[0] };
            let ar = if frame[1] < 0.0 { -frame[1] } else { frame[1] };
            let peak = if al > ar { al } else { ar };

            // Target gain to pull the peak down to the ceiling (1.0 if under).
            let target = if peak > THRESHOLD { THRESHOLD / peak } else { 1.0 };
            // Fast to clamp down, slow to recover.
            let coef = if target < self.gain { self.atk } else { self.rel };
            self.gain += coef * (target - self.gain);

            frame[0] *= self.gain;
            frame[1] *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn transparent_below_threshold() {
        let mut lim = Limiter::new(SR);
        let mut buf = vec![0.5, -0.4, 0.6, -0.3];
        let orig = buf.clone();
        lim.process(&mut buf);
        for (a, b) in buf.iter().zip(orig.iter()) {
            assert!((a - b).abs() < 1e-6, "should pass unchanged below ceiling");
        }
    }

    #[test]
    fn clamps_peaks_to_ceiling() {
        let mut lim = Limiter::new(SR);
        // Hot signal well above the ceiling; after the attack settles the
        // output magnitude must sit at/below THRESHOLD.
        let mut buf = vec![2.0_f32; 4096];
        lim.process(&mut buf);
        for &s in buf.iter().skip(2000) {
            assert!(s.abs() <= THRESHOLD + 1e-3, "peak should be limited, got {s}");
        }
    }
}
