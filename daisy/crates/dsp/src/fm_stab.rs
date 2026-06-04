//! FM "stab" voice — a small polyphonic bank for techno/industrial chord
//! hits, built from `infinitedsp` sine [`Oscillator`]s.
//!
//! Each voice is a 2-operator FM pair (one modulator sine bends a carrier
//! sine), plus the ingredients that push it from "clean DX bell" toward
//! "abrasive industrial":
//! - **operator self-feedback** — the carrier's own (averaged) output is fed
//!   back into its phase. As feedback rises the sine morphs toward a buzzing
//!   sawtooth; this is the classic DX "feedback operator" and the single
//!   biggest lever for grit.
//! - **inharmonic mod ratio** — non-integer ratios (e.g. 1.41, 3.5) give
//!   clangorous, metallic, detuned partials instead of musical harmonics.
//! - a **waveshaper** ([`Shaper`]) on the voice output — `HardClip` or
//!   `Foldback` add aggressive upper harmonics that a polite `Tanh` won't.
//!
//! Two envelopes shape it: an **amp** envelope (fast attack, exponential
//! decay, no sustain) for the percussive stab, and a **mod** envelope that
//! decays the FM index so the attack is bright and the tail mellows.
//!
//! Rendering is per-sample (`tick`), matching the engine's per-sample
//! sequencer loop. We drive `Oscillator::tick(freq_hz)` directly, so the
//! oscillator's internal `AudioParam`/block buffers are never touched and no
//! allocation happens after construction — safe for the embedded target.

use crate::svf::Svf;
use infinitedsp_core::FrameProcessor;
use infinitedsp_core::core::audio_param::AudioParam;
use infinitedsp_core::synthesis::oscillator::{Oscillator, Waveform};

/// Map a per-hit "tone" value (0..1) to the modulation applied on top of the
/// base patch. The three perceptual axes co-vary on one knob:
/// 0 = dark / short / heavily-filtered, 1 = bright / long / open.
///
/// Returns `(index_mul, decay_mul, cutoff_hz, resonance)`:
/// - `index_mul` scales FM index (brightness / sideband richness),
/// - `decay_mul` scales the amp + mod decay (note length),
/// - `cutoff_hz` is the per-note lowpass cutoff (exp-mapped, dark→open),
/// - `resonance` adds growl at the dark end where the filter is doing work.
fn tone_to_mods(tone: f32) -> (f32, f32, f32, f32) {
    let t = tone.clamp(0.0, 1.0);
    let index_mul = 0.4 + t * 1.2; // 0.4 .. 1.6
    let decay_mul = 0.3 + t * 1.7; // 0.3 .. 2.0
    let cutoff_hz = 180.0 * libm::powf(40.0, t); // ~180 Hz .. ~7.2 kHz (exp)
    let resonance = 0.15 + (1.0 - t) * 0.3; // darker hits ring a touch more
    (index_mul, decay_mul, cutoff_hz, resonance)
}

/// Polyphony. A triad uses 3 voices, so 8 lets two stabs overlap with room.
pub const NUM_VOICES: usize = 8;

/// Standard equal-temperament MIDI-note → frequency. Note 69 = A4 = 440 Hz.
pub fn midi_to_freq(note: u8) -> f32 {
    440.0 * libm::powf(2.0, (note as f32 - 69.0) / 12.0)
}

/// Output waveshaper applied per-voice after FM synthesis. The abrasive
/// options (`HardClip`, `Foldback`) are what give the "industrial" edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shaper {
    /// No shaping — the raw FM output.
    Off,
    /// Soft tanh saturation — warm, rounded (the "clean" option).
    Tanh,
    /// Hard clip to [-1, 1] — square-ish, buzzy, aggressive.
    HardClip,
    /// Sine foldback — wild inharmonic upper partials, very metallic/harsh.
    Foldback,
}

impl Shaper {
    #[inline]
    fn apply(self, x: f32) -> f32 {
        match self {
            Shaper::Off => x,
            Shaper::Tanh => libm::tanhf(x),
            Shaper::HardClip => x.clamp(-1.0, 1.0),
            Shaper::Foldback => libm::sinf(x),
        }
    }
}

/// Tunable timbre/shape shared by every voice in a [`FmStab`].
#[derive(Debug, Clone, Copy)]
pub struct FmPatch {
    /// Modulator frequency as a multiple of the carrier (the note). Integer
    /// ratios → harmonic/brassy; non-integer → metallic/clangorous/inharmonic.
    pub mod_ratio: f32,
    /// FM index — peak frequency deviation is `index * mod_freq`. ~1-4 musical,
    /// higher = more sidebands / more aggressive.
    pub index: f32,
    /// Carrier operator self-feedback (0..~1.2). 0 = pure sine; rising values
    /// morph the carrier toward a buzzing sawtooth. The main "grit" knob.
    pub feedback: f32,
    /// Pre-shaper drive (input gain into the waveshaper). 1.0 = unity.
    pub drive: f32,
    /// Output waveshaper. [`Shaper::HardClip`]/[`Shaper::Foldback`] = abrasive.
    pub shaper: Shaper,
    /// Amp-envelope attack, seconds. Keep tiny (~1-5 ms) for a stab.
    pub attack_s: f32,
    /// Amp-envelope decay to silence, seconds (the stab's length).
    pub decay_s: f32,
    /// Mod-envelope decay, seconds. Usually shorter than `decay_s` so the
    /// brightness fades faster than the tone.
    pub mod_decay_s: f32,
}

impl Default for FmPatch {
    /// A bright-but-warm minor-stab default: harmonic 1:1 FM, medium pluck,
    /// soft tanh — the "clean" DX-ish stab.
    fn default() -> Self {
        FmPatch {
            mod_ratio: 1.0,
            index: 2.2,
            feedback: 0.0,
            drive: 1.0,
            shaper: Shaper::Tanh,
            attack_s: 0.002,
            decay_s: 0.28,
            mod_decay_s: 0.12,
        }
    }
}

impl FmPatch {
    /// An abrasive industrial stab: inharmonic ratio, heavy operator feedback,
    /// high FM index, driven hard into a hard-clipper. Clangorous and metallic
    /// rather than musical.
    pub fn industrial() -> Self {
        FmPatch {
            mod_ratio: 1.414, // √2 — deliberately inharmonic, metallic
            index: 4.5,       // lots of sidebands
            feedback: 0.45,   // carrier sawtooth-buzz
            drive: 1.5,       // slam the shaper
            shaper: Shaper::HardClip,
            attack_s: 0.081, // slight swell
            decay_s: 1.32,
            mod_decay_s: 1.18, // brightness lingers — stays harsh through the tail
        }
    }

    /// A high, clean FM bell — Chowning's classic inharmonic bell recipe.
    ///
    /// The ~1.4:1 (inharmonic) mod ratio gives the metallic, clangorous
    /// partials a real bell has; the mod envelope decays the FM index quickly,
    /// so the strike rings bright and the long tail settles toward a near-pure
    /// sine. No grit (zero feedback) and no waveshaping — pure-sine FM, which
    /// auditioned identical to a tanh shaper here and saves a per-sample
    /// `tanhf` on the Daisy.
    ///
    /// Envelope: low attack (~4 ms mallet strike), long exponential ring-out.
    /// This voice has no sustain/release *stage* — the long `decay_s` is the
    /// ring: its early seconds read as the sustained body, the tail as a long
    /// release. Strike it a few octaves above A440 for the "high bell" — e.g.
    /// MIDI note 93 = A6 = 1760 Hz (A440 up two octaves), or 105 = A7 = 3520 Hz.
    pub fn bell() -> Self {
        FmPatch {
            mod_ratio: 1.4,       // inharmonic — Chowning's bell ratio, metallic partials
            index: 5.0,           // bright, clangorous strike...
            feedback: 0.0,        // clean — a bell is near-sinusoidal, no grit
            drive: 1.0,           // unity into the shaper
            shaper: Shaper::Off, // pure-sine FM — auditioned identical to Tanh, no per-sample tanhf
            attack_s: 0.004,      // ~4 ms — a quick mallet strike (low attack)
            decay_s: 5.0,         // very long ring-out (sustained body → long release)
            mod_decay_s: 0.6,     // ...that quickly mellows toward a pure sine tail
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
}

struct FmVoice {
    carrier: Oscillator,
    modulator: Oscillator,
    car_freq: f32,
    patch: FmPatch,

    stage: Stage,
    amp: f32,
    attack_inc: f32,
    decay_coeff: f32,
    mod_env: f32,
    mod_decay_coeff: f32,

    /// Averaged last-two carrier outputs, for the self-feedback path. DX-style
    /// two-sample averaging tames the instability raw feedback would cause.
    fb_z1: f32,
    fb_z2: f32,

    /// Per-voice resonant lowpass. Only run when `filtered` is set (a hit with
    /// a `stabtone:` value); otherwise bypassed so the clean DX pluck is
    /// untouched and costs nothing in the hot loop.
    svf: Svf,
    filtered: bool,

    velocity: f32,
    note: u8,
    /// Monotonic stamp from the bank, for oldest-voice stealing.
    age: u64,
}

impl FmVoice {
    fn new(sample_rate: f32) -> Self {
        let mut carrier = Oscillator::new(AudioParam::hz(1.0), Waveform::Sine);
        let mut modulator = Oscillator::new(AudioParam::hz(1.0), Waveform::Sine);
        carrier.set_sample_rate(sample_rate);
        modulator.set_sample_rate(sample_rate);
        FmVoice {
            carrier,
            modulator,
            car_freq: 0.0,
            patch: FmPatch::default(),
            stage: Stage::Idle,
            amp: 0.0,
            attack_inc: 1.0,
            decay_coeff: 0.0,
            mod_env: 0.0,
            mod_decay_coeff: 0.0,
            fb_z1: 0.0,
            fb_z2: 0.0,
            svf: Svf::new(sample_rate),
            filtered: false,
            velocity: 0.0,
            note: 0,
            age: 0,
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.carrier.set_sample_rate(sample_rate);
        self.modulator.set_sample_rate(sample_rate);
        self.svf = Svf::new(sample_rate);
    }

    fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    /// Strike a note. `tone` is the optional per-hit character (0..1): `None`
    /// plays the base patch verbatim with the filter bypassed (the pristine
    /// pluck); `Some(t)` co-varies brightness/length/filtering via
    /// [`tone_to_mods`].
    fn note_on(
        &mut self,
        note: u8,
        velocity: f32,
        patch: &FmPatch,
        tone: Option<f32>,
        sample_rate: f32,
        age: u64,
    ) {
        self.note = note;
        self.car_freq = midi_to_freq(note);
        self.patch = *patch;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.age = age;

        // Restart phases so stacked chord notes attack coherently.
        self.carrier.set_phase(0.0);
        self.modulator.set_phase(0.0);
        self.fb_z1 = 0.0;
        self.fb_z2 = 0.0;

        // Per-hit tone shaping. When absent, leave the patch untouched and the
        // filter bypassed so the base pluck is bit-for-bit unchanged.
        let (mut decay_s, mut mod_decay_s) = (patch.decay_s, patch.mod_decay_s);
        if let Some(t) = tone {
            let (index_mul, decay_mul, cutoff_hz, res) = tone_to_mods(t);
            self.patch.index = patch.index * index_mul;
            decay_s = patch.decay_s * decay_mul;
            mod_decay_s = patch.mod_decay_s * decay_mul;
            self.svf.set_freq(cutoff_hz);
            self.svf.set_res(res);
            self.filtered = true;
        } else {
            self.filtered = false;
        }

        self.amp = 0.0;
        self.stage = Stage::Attack;
        let attack_samples = (patch.attack_s * sample_rate).max(1.0);
        self.attack_inc = 1.0 / attack_samples;

        // Exponential coefficient matching infinitedsp's ADSR convention
        // (reaches ~95% of the target over the stated time).
        self.decay_coeff = exp_coeff(decay_s, sample_rate);
        self.mod_env = 1.0;
        self.mod_decay_coeff = exp_coeff(mod_decay_s, sample_rate);
    }

    #[inline]
    fn tick(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => return 0.0,
            Stage::Attack => {
                self.amp += self.attack_inc;
                if self.amp >= 1.0 {
                    self.amp = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.amp *= self.decay_coeff;
                if self.amp < 1e-4 {
                    self.amp = 0.0;
                    self.stage = Stage::Idle;
                    return 0.0;
                }
            }
        }

        self.mod_env *= self.mod_decay_coeff;
        let mod_freq = self.car_freq * self.patch.mod_ratio;
        let m = self.modulator.tick(mod_freq);

        // Carrier frequency deviation = FM from the modulator + operator
        // self-feedback (averaged last-two carrier output). Both scale with
        // the carrier frequency so the timbre tracks pitch.
        let fm_dev = self.patch.index * self.mod_env * mod_freq * m;
        let fb_avg = 0.5 * (self.fb_z1 + self.fb_z2);
        let fb_dev = self.patch.feedback * self.car_freq * fb_avg;

        let raw = self.carrier.tick(self.car_freq + fm_dev + fb_dev);
        // Update feedback history with the *pre-shaper* carrier sine.
        self.fb_z2 = self.fb_z1;
        self.fb_z1 = raw;

        // Drive into the waveshaper.
        let mut shaped = self.patch.shaper.apply(raw * self.patch.drive);

        // Optional per-hit resonant lowpass (only for hits with a tone value;
        // bypassed otherwise so the clean pluck path is untouched).
        if self.filtered {
            self.svf.process(shaped);
            shaped = self.svf.low();
        }

        shaped * self.amp * self.velocity
    }
}

/// Exponential one-pole decay coefficient that falls to ~5% over `time_s`.
fn exp_coeff(time_s: f32, sample_rate: f32) -> f32 {
    let samples = (time_s * sample_rate).max(1.0);
    libm::expf(-1.0 / (samples / 3.0))
}

/// A small polyphonic bank of [`FmVoice`]s sharing one [`FmPatch`].
pub struct FmStab {
    voices: [FmVoice; NUM_VOICES],
    patch: FmPatch,
    sample_rate: f32,
    gain: f32,
    counter: u64,
}

impl FmStab {
    pub fn new(sample_rate: f32) -> Self {
        FmStab {
            voices: core::array::from_fn(|_| FmVoice::new(sample_rate)),
            patch: FmPatch::default(),
            sample_rate,
            gain: 0.35,
            counter: 0,
        }
    }

    /// Start one note, allocating a free voice or stealing the oldest. Plays
    /// the base patch with the filter bypassed (the pristine pluck).
    pub fn note_on(&mut self, note: u8, velocity: f32) {
        self.note_on_toned(note, velocity, None);
    }

    /// Start one note with an optional per-hit `tone` (0..1). `None` =
    /// pristine; `Some(t)` co-varies brightness/length/filtering.
    pub fn note_on_toned(&mut self, note: u8, velocity: f32, tone: Option<f32>) {
        self.counter = self.counter.wrapping_add(1);
        let age = self.counter;
        let idx = self.pick_voice();
        self.voices[idx].note_on(note, velocity, &self.patch, tone, self.sample_rate, age);
    }

    /// Trigger every note of a chord at once (pristine, no per-hit tone).
    pub fn play_chord(&mut self, notes: &[u8], velocity: f32) {
        self.play_chord_toned(notes, velocity, None);
    }

    /// Trigger a chord with an optional shared per-hit `tone` applied to every
    /// note of the chord.
    pub fn play_chord_toned(&mut self, notes: &[u8], velocity: f32, tone: Option<f32>) {
        for &n in notes {
            self.note_on_toned(n, velocity, tone);
        }
    }

    fn pick_voice(&self) -> usize {
        // Prefer an idle voice; otherwise steal the oldest-started one.
        let mut best = 0usize;
        let mut best_age = u64::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            if v.is_idle() {
                return i;
            }
            if v.age < best_age {
                best_age = v.age;
                best = i;
            }
        }
        best
    }

    /// Render and sum all active voices for one sample (mono).
    #[inline]
    pub fn tick(&mut self) -> f32 {
        let mut sum = 0.0;
        for v in self.voices.iter_mut() {
            sum += v.tick();
        }
        sum * self.gain
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for v in self.voices.iter_mut() {
            v.set_sample_rate(sample_rate);
        }
    }

    /// Replace the whole patch (timbre). Takes effect on the next note-on —
    /// already-ringing voices keep the patch they were struck with.
    pub fn load_patch(&mut self, patch: FmPatch) {
        self.patch = patch;
    }

    pub fn patch(&self) -> &FmPatch {
        &self.patch
    }
    pub fn patch_mut(&mut self) -> &mut FmPatch {
        &mut self.patch
    }
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain.max(0.0);
    }
    pub fn gain(&self) -> f32 {
        self.gain
    }
    pub fn set_mod_ratio(&mut self, r: f32) {
        self.patch.mod_ratio = r.max(0.0);
    }
    pub fn set_index(&mut self, i: f32) {
        self.patch.index = i.max(0.0);
    }
    pub fn set_decay(&mut self, s: f32) {
        self.patch.decay_s = s.max(0.001);
    }
    /// Operator self-feedback amount — the main grit/abrasion control.
    pub fn set_feedback(&mut self, fb: f32) {
        self.patch.feedback = fb.max(0.0);
    }
    /// Pre-shaper drive.
    pub fn set_drive(&mut self, d: f32) {
        self.patch.drive = d.max(0.0);
    }
    /// Output waveshaper.
    pub fn set_shaper(&mut self, s: Shaper) {
        self.patch.shaper = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_is_440() {
        assert!((midi_to_freq(69) - 440.0).abs() < 0.001);
    }

    #[test]
    fn idle_bank_is_silent() {
        let mut bank = FmStab::new(48_000.0);
        for _ in 0..64 {
            assert_eq!(bank.tick(), 0.0);
        }
    }

    #[test]
    fn struck_voice_makes_sound_then_decays_to_silence() {
        let mut bank = FmStab::new(48_000.0);
        bank.set_decay(0.05);
        bank.note_on(60, 1.0);
        let mut peak = 0.0f32;
        for _ in 0..4_800 {
            // 100 ms
            peak = peak.max(bank.tick().abs());
        }
        assert!(peak > 0.0, "a struck stab should produce signal");
        // Let it fully ring out (1 s ≫ the 50 ms decay), then confirm the
        // voice has returned to exact silence (it goes Idle below threshold).
        for _ in 0..48_000 {
            bank.tick();
        }
        let mut tail = 0.0f32;
        for _ in 0..480 {
            tail += bank.tick().abs();
        }
        assert!(tail == 0.0, "stab should decay to silence, got {tail}");
    }

    #[test]
    fn voice_stealing_caps_active_voices() {
        let mut bank = FmStab::new(48_000.0);
        // Fire more notes than voices; should not panic and stays bounded.
        for n in 48..48 + (NUM_VOICES as u8 + 4) {
            bank.note_on(n, 1.0);
        }
        let active = bank.voices.iter().filter(|v| !v.is_idle()).count();
        assert!(active <= NUM_VOICES);
    }

    #[test]
    fn industrial_patch_stays_bounded_and_is_harsher() {
        // The hard-clipped industrial patch must not blow up, and should carry
        // more high-frequency energy (zero crossings) than the clean default.
        fn zero_crossings(patch: FmPatch) -> u32 {
            let mut bank = FmStab::new(48_000.0);
            bank.load_patch(patch);
            bank.note_on(60, 1.0);
            let mut prev = 0.0f32;
            let mut xings = 0u32;
            for _ in 0..4_800 {
                let s = bank.tick();
                assert!(s.abs() <= 1.5, "industrial output should stay bounded");
                if (s > 0.0) != (prev > 0.0) {
                    xings += 1;
                }
                prev = s;
            }
            xings
        }
        let clean = zero_crossings(FmPatch::default());
        let dirty = zero_crossings(FmPatch::industrial());
        assert!(
            dirty > clean,
            "industrial ({dirty}) should be brighter/harsher than clean ({clean})"
        );
    }

    #[test]
    fn bell_is_high_and_rings_far_longer_than_the_default_pluck() {
        // Tuning: A440 up two octaves = A6 = MIDI 93 = 1760 Hz.
        assert!((midi_to_freq(93) - 1760.0).abs() < 0.01, "note 93 should be A6 = 1760 Hz");

        fn tail_energy(patch: FmPatch) -> f64 {
            let mut bank = FmStab::new(48_000.0);
            bank.load_patch(patch);
            bank.note_on(93, 1.0); // high bell, A6
            // Skip the first 2 s, then measure energy in the following 200 ms.
            for _ in 0..(48_000 * 2) {
                bank.tick();
            }
            let mut e = 0.0f64;
            for _ in 0..9_600 {
                let s = bank.tick();
                e += (s * s) as f64;
            }
            e
        }

        let bell_tail = tail_energy(FmPatch::bell());
        let pluck_tail = tail_energy(FmPatch::default());
        // The default pluck (decay_s 0.28) is long dead by 2 s; the bell
        // (decay_s 5.0) should still be ringing with real energy.
        assert!(bell_tail > 0.0, "the bell should still ring ~2 s after the strike");
        assert!(
            bell_tail > pluck_tail * 100.0,
            "bell tail ({bell_tail:.6}) should dwarf the default pluck tail ({pluck_tail:.6})"
        );
    }

    /// Energy + brightness (zero crossings) of one struck note over `n` samples.
    fn measure(tone: Option<f32>, n: usize) -> (f64, u32) {
        let mut bank = FmStab::new(48_000.0);
        // Use the clean default pluck so the filter/tone is the only variable.
        bank.load_patch(FmPatch::default());
        bank.note_on_toned(60, 1.0, tone);
        let (mut energy, mut prev, mut xings) = (0.0f64, 0.0f32, 0u32);
        for _ in 0..n {
            let s = bank.tick();
            energy += (s * s) as f64;
            if (s > 0.0) != (prev > 0.0) {
                xings += 1;
            }
            prev = s;
        }
        (energy, xings)
    }

    #[test]
    fn dark_tone_is_duller_and_shorter_than_bright_tone() {
        // Brightness: a high-tone hit should have more zero crossings (more HF
        // through the open filter + higher FM index) than a low-tone hit.
        let (_, dark_x) = measure(Some(0.1), 4_800);
        let (_, bright_x) = measure(Some(0.95), 4_800);
        assert!(
            bright_x > dark_x,
            "bright tone ({bright_x} xings) should be brighter than dark ({dark_x})"
        );

        // Length: over a 1 s window the short (dark) hit should have decayed to
        // far less total energy than the long (bright) one.
        let (dark_e, _) = measure(Some(0.1), 48_000);
        let (bright_e, _) = measure(Some(0.95), 48_000);
        assert!(
            bright_e > dark_e * 2.0,
            "bright/long ({bright_e:.3}) should ring much longer than dark/short ({dark_e:.3})"
        );
    }

    #[test]
    fn no_tone_is_bit_for_bit_the_pristine_pluck() {
        // A hit with tone=None must be identical to the old filterless path:
        // render the patch directly and compare sample-for-sample.
        let mut toned = FmStab::new(48_000.0);
        toned.note_on_toned(60, 1.0, None);
        let mut plain = FmStab::new(48_000.0);
        plain.note_on(60, 1.0); // delegates to note_on_toned(.., None)
        for i in 0..4_800 {
            let a = toned.tick();
            let b = plain.tick();
            assert_eq!(a, b, "pristine path diverged at sample {i}");
        }
    }
}
