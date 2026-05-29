//! Resonant "bloom" bank — a small set of band-pass [`Svf`]s tuned to a fixed
//! chord (D Lydian) that ring pitched tone *out of* the program material when
//! excited. Fed from the pre-tape master tap, so its input is the rain / street
//! noise / pads of the composition itself — broadband noise into a high-Q
//! band-pass is the classic way to bloom a pitch out of nothing, so no new
//! notes are introduced: the track resonates itself.
//!
//! Driven by a single `amount` in `[0, 1]` ("proximity"):
//! - `0.0` — far: resonators at low Q, every voice's envelope at zero → silent.
//! - `1.0` — near: high Q, all voices open, the source *sings* into the chord.
//!
//! Voices enter progressively as `amount` rises (`onset`/`width` per voice).
//! The final voice is the Lydian **#4 (G#)** — its `onset` is high so it only
//! blooms in the closest band, surfacing the floating Lydian color as a payoff
//! when you lean all the way in, and dropping out first as you withdraw.
//!
//! Per-sample work is `NUM_VOICES` double-sampled SVF ticks and needs **no
//! delay buffer**, so unlike the ping-pong delay it has no SDRAM concern on the
//! embedded target.

use crate::svf::Svf;

/// Number of resonators in the bank.
pub const NUM_VOICES: usize = 6;

/// `(frequency_hz, mix_gain, onset, width)` per resonator, tuned to a
/// D Lydian chord (`D E F# G# A B C#`) at A4 = 440 Hz. `onset` is the `amount`
/// at which the voice starts to enter; it ramps to full over `width`.
const VOICES: [(f32, f32, f32, f32); NUM_VOICES] = [
    (146.83, 1.00, 0.00, 0.50), // D3  — root, low body (enters first)
    (220.00, 0.90, 0.05, 0.50), // A3  — fifth
    (293.66, 0.85, 0.10, 0.50), // D4  — root octave
    (369.99, 0.75, 0.25, 0.50), // F#4 — major third (color)
    (440.00, 0.75, 0.30, 0.50), // A4  — fifth / presence
    (415.30, 0.85, 0.80, 0.20), // G#4 — #4 Lydian signature, near-band only
];

/// Resonance (Q) at `amount` 0 and 1. Higher = more ringing / more pitched.
/// Capped below 1.0 for stability (the SVF's cubic damping self-limits above).
const RES_MIN: f32 = 0.50;
const RES_MAX: f32 = 0.90;

/// Attenuation applied to the program tap before the resonators — the master
/// mix is near full-scale and high-Q band-pass gain is large.
const INPUT_ATTEN: f32 = 0.35;

/// Overall bloom output gain folded back into the master. Tune by ear.
const MASTER_GAIN: f32 = 0.35;

struct Voice {
    svf: Svf,
    gain: f32,
    onset: f32,
    width: f32,
    /// Cached entry envelope, recomputed in [`BloomBank::set_amount`].
    env: f32,
}

pub struct BloomBank {
    voices: [Voice; NUM_VOICES],
    amount: f32,
}

impl BloomBank {
    pub fn new(sample_rate: f32) -> Self {
        let mk = |spec: (f32, f32, f32, f32)| {
            let mut svf = Svf::new(sample_rate);
            svf.set_freq(spec.0);
            svf.set_res(RES_MIN);
            Voice {
                svf,
                gain: spec.1,
                onset: spec.2,
                width: spec.3,
                env: 0.0,
            }
        };
        Self {
            voices: [
                mk(VOICES[0]),
                mk(VOICES[1]),
                mk(VOICES[2]),
                mk(VOICES[3]),
                mk(VOICES[4]),
                mk(VOICES[5]),
            ],
            amount: 0.0,
        }
    }

    /// Set the "proximity" amount in `[0, 1]`. Updates every resonator's Q and
    /// its entry envelope. Cheap — call per audio block (or per control tick).
    pub fn set_amount(&mut self, amount: f32) {
        let a = amount.clamp(0.0, 1.0);
        self.amount = a;
        let res = RES_MIN + (RES_MAX - RES_MIN) * a;
        for v in &mut self.voices {
            v.svf.set_res(res);
            v.env = ((a - v.onset) / v.width).clamp(0.0, 1.0);
        }
    }

    pub fn amount(&self) -> f32 {
        self.amount
    }

    /// Process one mono input sample; returns the bloom contribution to add
    /// back into the master. Resonators always run (cheap), but each voice's
    /// output is gated by its envelope, so the return is zero when `amount` is
    /// low — the caller can skip the whole bus when `amount() == 0.0`.
    pub fn tick(&mut self, input: f32) -> f32 {
        let x = input * INPUT_ATTEN;
        let mut sum = 0.0;
        for v in &mut self.voices {
            v.svf.process(x);
            sum += v.svf.band() * v.gain * v.env;
        }
        sum * MASTER_GAIN
    }
}
