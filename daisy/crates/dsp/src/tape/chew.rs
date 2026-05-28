//! Chew — tape dropouts modeled as random-interval volume-and-tone dips.
//!
//! **Revised design (post-feedback):** the original port mirrored CHOWTape's
//! `sign(x)·|x|^p` power-law shaper, but that introduced an audible
//! "static" / digital-crunch character at the moment the shaper engaged.
//! For our use case the user wants the "pumping" feel (rhythmic dips in
//! level + tone) without the shaper artifact, so we replace the shaper
//! with a smoothed gain attenuation and keep the LPF cutoff sweep.
//!
//! Per-state-flip the dry/wet envelope is interpolated over ~20 ms so
//! transitions don't click. The LPF coefficient and the gain are both
//! driven from the same envelope so the dip is single-stage.

use core::f32::consts::TAU;
use libm::expf;

const MIN_DRY_SECS: f32 = 0.3;
const MAX_DRY_SECS: f32 = 4.0;
const MIN_WET_SECS: f32 = 0.05;
const MAX_WET_SECS: f32 = 0.30;
const WET_FC_MIN_HZ: f32 = 5_000.0;
/// Envelope ramp time constant — long enough to remove the state-flip
/// click, short enough that the dip's onset still feels rhythmic.
const ENV_TC_SECS: f32 = 0.020;
/// At full depth, the wet state attenuates the signal by this fraction.
const MAX_GAIN_DROP: f32 = 0.5;

pub struct Chew {
    enabled: bool,
    sample_rate: f32,

    // State machine.
    is_crinkled: bool,
    sample_counter: u32,
    samples_until_change: u32,

    // Smoothed dry/wet envelope (0 = dry, 1 = full crinkle).
    env: f32,
    env_smooth: f32,

    // Per-channel one-pole LPF state.
    lpf_l: f32,
    lpf_r: f32,
    alpha_dry: f32,
    alpha_wet: f32,

    rng_state: u32,

    // User params, all 0..1.
    depth: f32,
    freq: f32,
    variance: f32,
}

impl Chew {
    pub fn new(sample_rate: f32) -> Self {
        let high_fc = 0.49 * sample_rate;
        let alpha = 1.0 - expf(-TAU * high_fc / sample_rate);
        let env_smooth = 1.0 - expf(-1.0 / (ENV_TC_SECS * sample_rate));
        Self {
            enabled: true,
            sample_rate,
            is_crinkled: false,
            sample_counter: 0,
            samples_until_change: 0,
            env: 0.0,
            env_smooth,
            lpf_l: 0.0,
            lpf_r: 0.0,
            alpha_dry: alpha,
            alpha_wet: alpha,
            rng_state: 0xCAFE_F00D,
            depth: 0.0,
            freq: 0.0,
            variance: 0.5,
        }
    }

    pub fn set_enabled(&mut self, en: bool) {
        self.enabled = en;
    }
    pub fn set_depth(&mut self, d: f32) {
        self.depth = d.clamp(0.0, 1.0);
        self.recompute_wet_alpha();
    }
    pub fn set_freq(&mut self, f: f32) {
        self.freq = f.clamp(0.0, 1.0);
    }
    pub fn set_variance(&mut self, v: f32) {
        self.variance = v.clamp(0.0, 1.0);
    }

    fn recompute_wet_alpha(&mut self) {
        let high_fc = 0.49 * self.sample_rate;
        let wet_fc = high_fc - (high_fc - WET_FC_MIN_HZ) * self.depth;
        self.alpha_wet = 1.0 - expf(-TAU * wet_fc / self.sample_rate);
    }

    #[inline]
    fn rand_u(&mut self) -> f32 {
        let mut x = self.rng_state.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        x as f32 / u32::MAX as f32
    }

    fn get_dry_samples(&mut self) -> u32 {
        let base = MIN_DRY_SECS + (MAX_DRY_SECS - MIN_DRY_SECS) * (1.0 - self.freq);
        let jitter = (self.rand_u() - 0.5) * 2.0 * self.variance * base;
        ((base + jitter).max(0.05) * self.sample_rate) as u32
    }

    fn get_wet_samples(&mut self) -> u32 {
        let base = MIN_WET_SECS + (MAX_WET_SECS - MIN_WET_SECS) * self.freq;
        let jitter = (self.rand_u() - 0.5) * 2.0 * self.variance * base;
        ((base + jitter).max(0.01) * self.sample_rate) as u32
    }

    /// Process one stereo frame in-place.
    pub fn process_sample(&mut self, l: &mut f32, r: &mut f32) {
        if !self.enabled || self.freq <= 0.0 || self.depth <= 0.0 {
            // Bypass through the LPF (kept current) and let env decay.
            self.is_crinkled = false;
            self.env *= 1.0 - self.env_smooth;
            self.lpf_l += self.alpha_dry * (*l - self.lpf_l);
            self.lpf_r += self.alpha_dry * (*r - self.lpf_r);
            *l = self.lpf_l;
            *r = self.lpf_r;
            return;
        }

        // State machine: flip when the segment timer expires.
        if self.sample_counter >= self.samples_until_change {
            self.sample_counter = 0;
            self.is_crinkled = !self.is_crinkled;
            self.samples_until_change = if self.is_crinkled {
                self.get_wet_samples()
            } else {
                self.get_dry_samples()
            };
        }
        self.sample_counter = self.sample_counter.saturating_add(1);

        // Smoothed envelope toward the target state. ~20 ms ramp removes
        // the state-flip click and gives a "dip" rather than a "drop".
        let target = if self.is_crinkled { 1.0 } else { 0.0 };
        self.env += self.env_smooth * (target - self.env);

        // Gain attenuation: full depth = up to 50 % drop at peak crinkle.
        let gain = 1.0 - self.env * (self.depth * MAX_GAIN_DROP);

        // LPF coefficient blended from dry (full bandwidth) → wet (5 kHz floor).
        let alpha = self.alpha_dry + self.env * (self.alpha_wet - self.alpha_dry);

        // Apply per channel.
        let l_in = *l * gain;
        self.lpf_l += alpha * (l_in - self.lpf_l);
        *l = self.lpf_l;

        let r_in = *r * gain;
        self.lpf_r += alpha * (r_in - self.lpf_r);
        *r = self.lpf_r;
    }
}
