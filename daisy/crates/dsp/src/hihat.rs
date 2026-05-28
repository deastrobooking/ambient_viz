//! 808-style hi-hat — port of DaisySP `Source/Drums/hihat.cpp`/`.h`
//! (in turn ported from Plaits' `hihat.h`, originally by Emilie Gillet).
//!
//! Architecture:
//! - **6-square-oscillator "metallic noise"** ([`SquareNoise`]) at the
//!   classic Plaits frequency ratios — sums six biased phase carriers as
//!   the raw cymbal-like spectrum.
//! - **Band-pass SVF** colours the metallic noise (tone control).
//! - **Clocked random-sample mix** adds variety/grit ("not in the 808
//!   circuit", per Plaits' comment).
//! - **Envelope** with two-stage decay (fast initial drop above 0.5,
//!   slower tail below). Controls overall length.
//! - **High-pass SVF** finishes the output.
//!
//! Use one instance with a **short decay** for closed hat, another with a
//! **long decay** for open hat — same model, different decay setting.

use libm::powf;

use crate::svf::Svf;

const K_ONE_TWELFTH: f32 = 1.0 / 12.0;

/// 6 phase-accumulator square oscillators at Plaits' fixed ratios.
pub struct SquareNoise {
    phases: [u32; 6],
}

impl SquareNoise {
    pub fn new() -> Self {
        Self { phases: [0; 6] }
    }

    /// `f0` is *normalized* frequency (Hz / sample_rate).
    pub fn process(&mut self, f0: f32) -> f32 {
        // Plaits' ratios; nominal f0 = 414 Hz.
        const RATIOS: [f32; 6] = [1.0, 1.304, 1.466, 1.787, 1.932, 2.536];

        let mut noise: u32 = 0;
        for i in 0..6 {
            let f = (f0 * RATIOS[i]).min(0.499);
            let inc = (f * 4_294_967_296.0) as u32;
            self.phases[i] = self.phases[i].wrapping_add(inc);
            noise = noise.wrapping_add(self.phases[i] >> 31);
        }
        0.33 * (noise as f32) - 1.0
    }
}

impl Default for SquareNoise {
    fn default() -> Self {
        Self::new()
    }
}

pub struct HiHat {
    sample_rate: f32,

    // User-facing params.
    accent: f32,
    /// Normalized frequency (Hz / sample_rate).
    f0: f32,
    tone: f32,
    /// Post-remap decay (raw 0..1 → internal −1.2..0.5).
    decay: f32,
    noisiness: f32,
    sustain: bool,

    // State.
    trig: bool,
    envelope: f32,
    noise_clock: f32,
    noise_sample: f32,
    sustain_gain: f32,
    rng_state: u32,

    // Components.
    metallic: SquareNoise,
    bpf: Svf,
    hpf: Svf,
}

impl HiHat {
    pub fn new(sample_rate: f32) -> Self {
        let mut h = Self {
            sample_rate,
            accent: 0.0,
            f0: 0.0,
            tone: 0.0,
            decay: 0.0,
            noisiness: 0.0,
            sustain: false,
            trig: false,
            envelope: 0.0,
            noise_clock: 0.0,
            noise_sample: 0.0,
            sustain_gain: 0.0,
            rng_state: 0xC001_BEEF,
            metallic: SquareNoise::new(),
            bpf: Svf::new(sample_rate),
            hpf: Svf::new(sample_rate),
        };
        h.set_freq(3000.0);
        h.set_tone(0.5);
        h.set_decay(0.2);
        h.set_noisiness(0.8);
        h.set_accent(0.8);
        h
    }

    pub fn trig(&mut self) {
        self.trig = true;
    }

    pub fn set_freq(&mut self, hz: f32) {
        self.f0 = (hz / self.sample_rate).clamp(0.0, 1.0);
    }
    pub fn set_tone(&mut self, t: f32) {
        self.tone = t.clamp(0.0, 1.0);
    }
    /// Raw 0..1 (or higher), remapped by DaisySP convention to internal
    /// `−1.2..0.5` and used to drive a `1 − 0.003·2^(−decay·7)` per-sample
    /// envelope decay multiplier.
    pub fn set_decay(&mut self, d: f32) {
        self.decay = d.max(0.0) * 1.7 - 1.2;
    }
    pub fn set_noisiness(&mut self, n: f32) {
        let n = n.clamp(0.0, 1.0);
        self.noisiness = n * n;
    }
    pub fn set_accent(&mut self, a: f32) {
        self.accent = a.clamp(0.0, 1.0);
    }
    pub fn set_sustain(&mut self, s: bool) {
        self.sustain = s;
    }

    #[inline]
    fn semitones_to_ratio(x: f32) -> f32 {
        powf(2.0, x * K_ONE_TWELFTH)
    }

    /// xorshift32 → bipolar `[-0.5, 0.5)` (matches DaisySP's `rand*kRandFrac − 0.5`).
    #[inline]
    fn rand_bipolar(&mut self) -> f32 {
        let mut x = self.rng_state.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        (x as f32 / u32::MAX as f32) - 0.5
    }

    pub fn process(&mut self, trigger: bool) -> f32 {
        let envelope_decay = 1.0 - 0.003 * Self::semitones_to_ratio(-self.decay * 84.0);
        let cut_decay = 1.0 - 0.0025 * Self::semitones_to_ratio(-self.decay * 36.0);

        if trigger || self.trig {
            self.trig = false;
            self.envelope = (1.5 + 0.5 * (1.0 - self.decay)) * (0.3 + 0.7 * self.accent);
        }

        // Metallic noise.
        let mut out = self.metallic.process(2.0 * self.f0);

        // BPF tone shaping.
        let cutoff = (150.0 / self.sample_rate * Self::semitones_to_ratio(self.tone * 72.0))
            .clamp(0.0, 16000.0 / self.sample_rate);
        self.bpf.set_freq(cutoff * self.sample_rate);
        self.bpf.set_res(3.0 + 6.0 * self.tone);
        self.bpf.process(out);
        out = self.bpf.band();

        // Clocked random-sample mix (Plaits' "salt").
        let noise_f = (self.f0 * (16.0 + 16.0 * (1.0 - self.noisiness))).clamp(0.0, 0.5);
        self.noise_clock += noise_f;
        if self.noise_clock >= 1.0 {
            self.noise_clock -= 1.0;
            self.noise_sample = self.rand_bipolar();
        }
        out += self.noisiness * (self.noise_sample - out);

        // VCA — two-stage envelope decay.
        self.sustain_gain = self.accent * self.decay;
        self.envelope *= if self.envelope > 0.5 {
            envelope_decay
        } else {
            cut_decay
        };
        let gain = if self.sustain {
            self.sustain_gain
        } else {
            self.envelope
        };
        out *= gain;

        // HPF.
        self.hpf.set_freq(cutoff * self.sample_rate);
        self.hpf.set_res(0.5);
        self.hpf.process(out);
        out = self.hpf.high();

        out
    }
}
