//! Formant speech utterances, smeared through a reverb — the exhibit's
//! surveillance/dissociation voice.
//!
//! Wraps infinitedsp's [`SpeechSynth`] (a formant vocal synth driven by a
//! phoneme sequence) and a Schroeder [`Reverb`] into one occasional foreground
//! voice. Each [`trigger_phrase`] speaks one of a small set of short phrases
//! ("pain material", "you are alone", "i see you", "you do not belong here")
//! and rings out through a long reverb tail. Silent until triggered; renders a
//! stereo block summed onto the master, mirroring the FM bell on top of the mix.
//!
//! [`trigger_phrase`]: PainMaterialVoice::trigger_phrase

use alloc::vec::Vec;
use infinitedsp_core::FrameProcessor;
use infinitedsp_core::core::audio_param::AudioParam;
// Embedded reverb: i16 storage + 2x downsampling → ~half the per-sample CPU of
// the full Schroeder `Reverb`. The Daisy's M7 has no hardware SIMD (the comb
// filters' `wide::f32x4` runs scalar), so the full-rate reverb overran the audio
// callback and underran the SAI (audible crackle). Aliased as `Reverb` so the
// rest of this file is unchanged. Same API (new_with_params / FrameProcessor).
use infinitedsp_core::low_mem::effects::time::reverb_low_mem::ReverbLowMem as Reverb;
use infinitedsp_core::synthesis::speech::{Phoneme, SpeechSynth};

/// The speakable phrases, each spelled in the synth's fixed token vocabulary.
/// The caller picks one per trigger by index (e.g. at random). Edit the tokens
/// to reshape pronunciation — available tokens: vowels A E I O U EE AI;
/// consonants P T K B D G M N NG F V S Z TH SH CH J R L W Y H; and GAP (a
/// silence between words). These are rough formant approximations.
const PHRASES: &[&[&str]] = &[
    // "pain material" (ma-TEE-ree-a-l)
    &["P", "AI", "N", "GAP", "GAP", "M", "A", "T", "EE", "R", "I", "A", "L"],
    // "you are alone" ("u r alone")
    &["Y", "U", "GAP", "A", "R", "GAP", "GAP", "U", "L", "O", "N"],
    // "i see you"
    &["AI", "GAP", "S", "EE", "GAP", "Y", "U"],
    // "you do not belong here"
    &[
        "Y", "U", "GAP", "D", "U", "GAP", "N", "O", "T", "GAP", //
        "B", "I", "L", "O", "NG", "GAP", "H", "EE", "R",
    ],
    // "ha ha ha ha ha"
    &[
        "H", "A", "GAP", "H", "A", "GAP", "H", "A", "GAP", //
        "H", "A", "GAP", "H", "A",
    ],
    // "eins zwei drei vier" (German "ains tsvai drai feer")
    &[
        "AI", "N", "S", "GAP", "T", "S", "V", "AI", "GAP", //
        "D", "R", "AI", "GAP", "F", "EE", "R",
    ],
    // "don't come back"
    &["D", "O", "N", "T", "GAP", "K", "U", "M", "GAP", "B", "A", "K"],
    // "you are not welcome"
    &[
        "Y", "U", "GAP", "A", "R", "GAP", "N", "O", "T", "GAP", //
        "W", "E", "L", "K", "U", "M",
    ],
    // "you are not happy"
    &[
        "Y", "U", "GAP", "A", "R", "GAP", "N", "O", "T", "GAP", //
        "H", "A", "P", "EE",
    ],
    // "you can not feel joy"
    &[
        "Y", "U", "GAP", "K", "A", "N", "GAP", "N", "O", "T", "GAP", //
        "F", "EE", "L", "GAP", "J", "O", "I",
    ],
    // "you are fake"
    &["Y", "U", "GAP", "A", "R", "GAP", "F", "AI", "K"],
    // "everybody sees through you"
    &[
        "E", "V", "R", "EE", "GAP", "B", "O", "D", "EE", "GAP", //
        "S", "EE", "Z", "GAP", "TH", "R", "U", "GAP", "Y", "U",
    ],
    // "you are weak"
    &["Y", "U", "GAP", "A", "R", "GAP", "W", "EE", "K"],
];

/// Human-readable labels, parallel to [`PHRASES`] (for logs / auditioning).
pub const PHRASE_LABELS: &[&str] = &[
    "pain material",
    "you are alone",
    "i see you",
    "you do not belong here",
    "ha ha ha ha ha",
    "eins zwei drei vier",
    "don't come back",
    "you are not welcome",
    "you are not happy",
    "you can not feel joy",
    "you are fake",
    "everybody sees through you",
    "you are weak",
];

/// Number of phrases the voice can speak. The trigger index wraps modulo this.
pub const PHRASE_COUNT: usize = PHRASES.len();

/// Tail (seconds) the reverb is allowed to ring after the phrase finishes
/// before the voice deactivates and stops drawing CPU.
const REVERB_TAIL_S: f32 = 4.0;

fn build_phrase(tokens: &[&str]) -> Vec<Phoneme> {
    let mut seq = Vec::new();
    for tok in tokens {
        seq.extend_from_slice(Phoneme::from_token(tok));
    }
    seq
}

pub struct PainMaterialVoice {
    synth: SpeechSynth<'static>,
    reverb: Reverb,
    /// The speakable phrases, each a leaked 'static phoneme buffer; a trigger
    /// selects one by index.
    phrases: Vec<&'static [Phoneme]>,
    sample_rate: f32,
    /// Per-block mono scratch for the dry speech, sized once at construction.
    mono: Vec<f32>,
    /// Per-block stereo scratch holding the dry signal across the in-place
    /// reverb so wet and dry can be blended afterwards.
    dry: Vec<f32>,
    /// Wet/dry mix (0..1). High — the phrase should arrive mostly as a wash,
    /// with a little dry so the consonants still read. Tune on hardware.
    wet: f32,
    /// Output level, scaled by trigger velocity.
    gain: f32,
    /// True while sounding (phrase playing OR reverb tail still ringing).
    active: bool,
    /// Samples of reverb tail left to render after the phrase finished.
    tail_remaining: usize,
}

impl PainMaterialVoice {
    /// `max_stereo_block` = the largest interleaved-stereo block (in f32
    /// samples) that [`process`] will be handed, so the scratch buffers are
    /// sized once and never reallocate on the audio path.
    ///
    /// [`process`]: PainMaterialVoice::process
    pub fn new(sample_rate: f32, max_stereo_block: usize) -> Self {
        // Build each phrase once and leak it to 'static — one-time at startup,
        // off the audio path; the device runs forever so it's never freed.
        let phrases: Vec<&'static [Phoneme]> = PHRASES
            .iter()
            .map(|toks| {
                let leaked: &'static [Phoneme] = Vec::leak(build_phrase(toks));
                leaked
            })
            .collect();
        let mut synth = SpeechSynth::new(sample_rate);
        synth.set_sample_rate(sample_rate);
        // Left idle (is_finished) until the first trigger.

        // A big, smeared room — long tail, moderate damping.
        let mut reverb = Reverb::new_with_params(
            // 0.92 pushed the comb feedback to ~0.977 (near self-oscillation):
            // the low-mem reverb's i16 combs built up and clipped INTERNALLY,
            // distorting the tail. 0.65 (~0.9 feedback) keeps the buildup well
            // under full-scale — calmer tail, no internal clip. Dial 0.5..0.8.
            AudioParam::Static(0.65), // room size
            AudioParam::Static(0.35), // damping
            0,
        );
        reverb.set_sample_rate(sample_rate);

        let mut mono = Vec::new();
        mono.resize(max_stereo_block / 2 + 1, 0.0);
        let mut dry = Vec::new();
        dry.resize(max_stereo_block + 2, 0.0);

        Self {
            synth,
            reverb,
            phrases,
            sample_rate,
            mono,
            dry,
            wet: 0.85,
            gain: 0.9,
            active: false,
            tail_remaining: 0,
        }
    }

    /// Speak phrase `index` once from the top (wraps modulo [`PHRASE_COUNT`], so
    /// any value is safe). `velocity` (0..1) scales level.
    pub fn trigger_phrase(&mut self, index: usize, velocity: f32) {
        let phrase = self.phrases[index % self.phrases.len()];
        self.synth.set_phonemes(phrase);
        // Keep it clearly audible even on a gentle trigger.
        self.gain = 0.9 * velocity.clamp(0.0, 1.0).max(0.25);
        self.tail_remaining = (REVERB_TAIL_S * self.sample_rate) as usize;
        self.active = true;
    }

    /// Speak the first phrase ("pain material") — convenience for callers that
    /// don't choose a phrase.
    pub fn trigger(&mut self, velocity: f32) {
        self.trigger_phrase(0, velocity);
    }

    /// True while the voice is sounding (skip [`process`] when false to save
    /// CPU — its render and reverb run only while active).
    ///
    /// [`process`]: PainMaterialVoice::process
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Render one interleaved-stereo block in place: speech → reverb (wet/dry
    /// blend). When the phrase has finished the speech feeds silence so the
    /// reverb tail rings out, then the voice deactivates. Returns `is_active()`.
    pub fn process(&mut self, stereo: &mut [f32], sample_index: u64) -> bool {
        let frames = stereo.len() / 2;
        if !self.active || frames == 0 {
            stereo.fill(0.0);
            return self.active;
        }

        // 1) Dry mono speech (self-silences once the phrase ends).
        let mono = &mut self.mono[..frames];
        self.synth.process(mono, sample_index);

        // 2) Expand to a stereo dry signal at the trigger level.
        let dry = &mut self.dry[..frames * 2];
        for (i, &m) in mono.iter().enumerate() {
            let s = m * self.gain;
            dry[2 * i] = s;
            dry[2 * i + 1] = s;
            stereo[2 * i] = s;
            stereo[2 * i + 1] = s;
        }

        // 3) Reverb overwrites `stereo` with the fully-wet field; blend back the
        //    stashed dry so the words still cut through the wash.
        self.reverb.process(&mut stereo[..frames * 2], sample_index);
        let wet = self.wet;
        let dry_g = 1.0 - wet;
        for (o, &d) in stereo[..frames * 2].iter_mut().zip(dry.iter()) {
            *o = d * dry_g + *o * wet;
        }

        // 4) Once the phrase is done, run down the reverb tail then deactivate.
        if self.synth.is_finished() {
            self.tail_remaining = self.tail_remaining.saturating_sub(frames);
            if self.tail_remaining == 0 {
                self.active = false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_phrase_speaks_then_falls_silent_and_deactivates() {
        assert_eq!(PHRASE_COUNT, PHRASE_LABELS.len(), "labels parallel to phrases");

        let sr = 48_000.0;
        let block = 64; // interleaved stereo samples
        let mut v = PainMaterialVoice::new(sr, block);

        for phrase in 0..PHRASE_COUNT {
            assert!(!v.is_active(), "idle before trigger (phrase {phrase})");
            v.trigger_phrase(phrase, 1.0);
            assert!(v.is_active(), "active after trigger (phrase {phrase})");

            // Drive blocks and confirm it actually produces sound, then
            // deactivates (phrase + bounded reverb tail), without running forever.
            let mut buf = [0.0f32; 64];
            let mut idx = 0u64;
            let mut max_abs = 0.0f32;
            let mut blocks = 0u32;
            // Cap well above longest phrase(~3 s) + tail(4 s) -> ~5300 blocks @ 64.
            let cap = 30_000u32;
            while v.is_active() && blocks < cap {
                v.process(&mut buf, idx);
                for &s in buf.iter() {
                    let a = if s < 0.0 { -s } else { s };
                    if a > max_abs {
                        max_abs = a;
                    }
                    assert!(a.is_finite() && a < 8.0, "voice output stays bounded");
                }
                idx += (buf.len() / 2) as u64;
                blocks += 1;
            }
            assert!(max_abs > 1e-4, "phrase {phrase} produced audible output (peak {max_abs})");
            assert!(!v.is_active(), "phrase {phrase} deactivated after tail (blocks={blocks})");
        }
    }

    #[test]
    fn trigger_index_wraps() {
        let mut v = PainMaterialVoice::new(48_000.0, 64);
        v.trigger_phrase(PHRASE_COUNT + 1, 1.0); // out of range -> wraps, no panic
        assert!(v.is_active());
    }
}
