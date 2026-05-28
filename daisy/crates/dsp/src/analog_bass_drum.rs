//! Analog bass drum — 808-style, revisited.
//!
//! Ported from DaisySP `Source/Drums/analogbassdrum.cpp` (Ben Sergentanis),
//! which itself ports the model from `pichenettes/eurorack/plaits/dsp/drums/
//! analog_bass_drum.h` (Emilie Gillet, 2016).
//!
//! The model emulates a discrete circuit: an exciter pulse triggers a
//! highly-resonant filter (our `Svf`) tuned around the drum's fundamental,
//! with both FM modulation from the attack envelope and self-FM from the
//! filter's own low-pass output (the "punch").

use core::f32::consts::TAU;
use libm::{cosf, fabsf, powf, sinf};

use crate::svf::Svf;

const K_ONE_TWELFTH: f32 = 1.0 / 12.0;

pub struct AnalogBassDrum {
    sample_rate: f32,

    accent: f32,
    f0: f32,
    tone: f32,
    decay: f32,
    attack_fm_amount: f32,
    self_fm_amount: f32,

    trig: bool,
    sustain: bool,

    pulse_remaining_samples: i32,
    fm_pulse_remaining_samples: i32,
    pulse: f32,
    pulse_height: f32,
    pulse_lp: f32,
    fm_pulse_lp: f32,
    retrig_pulse: f32,
    lp_out: f32,
    tone_lp: f32,
    sustain_gain: f32,
    phase: f32,

    resonator: Svf,
}

impl AnalogBassDrum {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sample_rate,
            accent: 0.0,
            f0: 0.0,
            tone: 0.0,
            decay: 0.0,
            attack_fm_amount: 0.0,
            self_fm_amount: 0.0,
            trig: false,
            sustain: false,
            pulse_remaining_samples: 0,
            fm_pulse_remaining_samples: 0,
            pulse: 0.0,
            pulse_height: 0.0,
            pulse_lp: 0.0,
            fm_pulse_lp: 0.0,
            retrig_pulse: 0.0,
            lp_out: 0.0,
            tone_lp: 0.0,
            sustain_gain: 0.0,
            phase: 0.0,
            resonator: Svf::new(sample_rate),
        };
        // Defaults matching DaisySP's Init().
        s.set_sustain(false);
        s.set_accent(0.1);
        s.set_freq(50.0);
        s.set_tone(0.1);
        s.set_decay(0.3);
        s.set_self_fm_amount(1.0);
        s.set_attack_fm_amount(0.5);
        s
    }

    /// Strike the drum.
    pub fn trig(&mut self) {
        self.trig = true;
    }

    /// If true, the drum sustains indefinitely instead of decaying.
    pub fn set_sustain(&mut self, sustain: bool) {
        self.sustain = sustain;
    }

    /// Accent (loudness/punch boost). 0..1.
    pub fn set_accent(&mut self, accent: f32) {
        self.accent = accent.clamp(0.0, 1.0);
    }

    /// Root frequency in Hz.
    pub fn set_freq(&mut self, f0_hz: f32) {
        self.f0 = (f0_hz / self.sample_rate).clamp(0.0, 0.5);
    }

    /// Click amount. 0..1.
    pub fn set_tone(&mut self, tone: f32) {
        self.tone = tone.clamp(0.0, 1.0);
    }

    /// Decay length. Works best 0..1. (DaisySP remaps internally; values
    /// above 1 still work but get progressively unstable.)
    pub fn set_decay(&mut self, decay: f32) {
        // DaisySP: decay_ = decay * 0.1; decay_ -= 0.1;
        self.decay = decay * 0.1 - 0.1;
    }

    /// Attack FM amount. Works best 0..1.
    pub fn set_attack_fm_amount(&mut self, amount: f32) {
        self.attack_fm_amount = amount * 50.0;
    }

    /// Self-FM amount. Works best 0..1. Also colours decay and volume.
    pub fn set_self_fm_amount(&mut self, amount: f32) {
        self.self_fm_amount = amount * 50.0;
    }

    /// Soft-knee diode shaper. Linear above 0, smooth tanh-ish curve below.
    #[inline]
    fn diode(x: f32) -> f32 {
        if x >= 0.0 {
            x
        } else {
            let x = x * 2.0;
            0.7 * x / (1.0 + fabsf(x))
        }
    }

    /// Render one sample. `trigger=true` strikes the drum (alternative to
    /// `trig()` for inline triggering from a sequencer/MIDI handler).
    pub fn process(&mut self, trigger: bool) -> f32 {
        // Per-call constants (cheap; DaisySP recomputes them too).
        let k_trigger_pulse_duration = (1.0e-3 * self.sample_rate) as i32;
        let k_fm_pulse_duration = (6.0e-3 * self.sample_rate) as i32;
        let k_pulse_decay_time = 0.2e-3 * self.sample_rate;
        let k_pulse_filter_time = 0.1e-3 * self.sample_rate;
        let k_retrig_pulse_duration = 0.05 * self.sample_rate;

        let scale = 0.001 / self.f0;
        let q = 1500.0 * powf(2.0, K_ONE_TWELFTH * self.decay * 80.0);
        let tone_f = f32::min(
            4.0 * self.f0 * powf(2.0, K_ONE_TWELFTH * self.tone * 108.0),
            1.0,
        );
        let exciter_leak = 0.08 * (self.tone + 0.25);

        if trigger || self.trig {
            self.trig = false;
            self.pulse_remaining_samples = k_trigger_pulse_duration;
            self.fm_pulse_remaining_samples = k_fm_pulse_duration;
            self.pulse_height = 3.0 + 7.0 * self.accent;
            self.lp_out = 0.0;
        }

        // Q39 / Q40 — main exciter pulse, then exponential decay.
        let mut pulse;
        if self.pulse_remaining_samples != 0 {
            self.pulse_remaining_samples -= 1;
            pulse = if self.pulse_remaining_samples != 0 {
                self.pulse_height
            } else {
                self.pulse_height - 1.0
            };
            self.pulse = pulse;
        } else {
            self.pulse *= 1.0 - 1.0 / k_pulse_decay_time;
            pulse = self.pulse;
        }
        if self.sustain {
            pulse = 0.0;
        }

        // C40 / R163 / R162 / D83 — one-pole filter on pulse, then diode shaping.
        self.pulse_lp += (1.0 / k_pulse_filter_time) * (pulse - self.pulse_lp);
        pulse = Self::diode((pulse - self.pulse_lp) + pulse * 0.044);

        // Q41 / Q42 — FM modulation pulse, with retrig "kick" at the tail.
        let mut fm_pulse;
        if self.fm_pulse_remaining_samples != 0 {
            self.fm_pulse_remaining_samples -= 1;
            fm_pulse = 1.0;
            // C39 / C52 — release the retrig spike on the last sample.
            self.retrig_pulse = if self.fm_pulse_remaining_samples != 0 {
                0.0
            } else {
                -0.8
            };
        } else {
            fm_pulse = 0.0;
            // C39 / R161 — retrig pulse decays back to 0.
            self.retrig_pulse *= 1.0 - 1.0 / k_retrig_pulse_duration;
        }
        if self.sustain {
            fm_pulse = 0.0;
        }
        self.fm_pulse_lp += (1.0 / k_pulse_filter_time) * (fm_pulse - self.fm_pulse_lp);

        // Q43 + R170 leakage — "punch" derived from the resonator's LP output.
        let punch = 0.7 + Self::diode(10.0 * self.lp_out - 1.0);

        // Q43 / R165 — FM the resonator from attack envelope + self-feedback.
        let attack_fm = self.fm_pulse_lp * 1.7 * self.attack_fm_amount;
        let self_fm = punch * 0.08 * self.self_fm_amount;
        let f = (self.f0 * (1.0 + attack_fm + self_fm)).clamp(0.0, 0.4);

        let resonator_out;
        if self.sustain {
            // Bypass the resonator with a sin/cos oscillator pair when sustaining
            // (so the drum's frequency stays clean and infinite-length).
            self.sustain_gain = self.accent * self.decay;
            self.phase += f;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            resonator_out = sinf(TAU * self.phase) * self.sustain_gain;
            self.lp_out = cosf(TAU * self.phase) * self.sustain_gain;
        } else {
            self.resonator.set_freq(f * self.sample_rate);
            self.resonator.set_res(0.4 * q * f);
            self.resonator
                .process((pulse - self.retrig_pulse * 0.2) * scale);
            resonator_out = self.resonator.band();
            self.lp_out = self.resonator.low();
        }

        // Final tone-shaping filter.
        self.tone_lp += tone_f * (pulse * exciter_leak + resonator_out - self.tone_lp);
        self.tone_lp
    }
}
