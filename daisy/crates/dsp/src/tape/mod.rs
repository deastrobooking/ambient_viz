//! Analog tape emulation — port-in-progress of CHOWTape.
//! See `TAPE_SIMULATION.md` at the workspace root for the full plan.
//!
//! **Phase 1 (this file):** head bump peak filter + low-passed hiss.
//! Both run on the master bus *after* the reverb wet/dry blend, applied
//! to the final interleaved-stereo buffer in `Engine::process`.
//!
//! Phases 2-5 (wow/flutter, loss filter, hysteresis, chew) will land in
//! sibling files and be chained inside [`TapeProcessor::process`].

use alloc::vec::Vec;

use infinitedsp_core::FrameProcessor;
use infinitedsp_core::core::audio_param::AudioParam;
use infinitedsp_core::effects::dynamics::compressor::Compressor;
use infinitedsp_core::effects::filter::biquad::{Biquad, FilterType};
use infinitedsp_core::synthesis::oscillator::{Oscillator, Waveform};

pub mod chew;
pub mod hysteresis;
pub mod loss_filter;
pub mod wow_flutter;
pub use chew::Chew;
pub use hysteresis::Hysteresis;
pub use loss_filter::LossFilter;
pub use wow_flutter::WowFlutter;

/// Default head-bump centre frequency (Hz). Real tape's bump frequency
/// depends on tape speed and head-gap size; 80 Hz is a reasonable
/// "quarter-inch at 7.5 ips" default.
const DEFAULT_BUMP_HZ: f32 = 80.0;
/// Default head-bump Q. CHOWTape uses 2.0 internally.
const DEFAULT_BUMP_Q: f32 = 2.0;
/// Default head-bump gain (dB).
const DEFAULT_BUMP_GAIN_DB: f32 = 3.0;
/// Default hiss low-pass cutoff (Hz). Tape noise rolls off above the
/// loss-filter knee; ~7 kHz approximates that without the full filter.
const DEFAULT_HISS_LPF_HZ: f32 = 7000.0;
/// Default hiss level, linear. ~-55 dB — present but quiet on a clean
/// signal, more audible when the music is sparse (just like real tape).
const DEFAULT_HISS_AMOUNT: f32 = 0.0018;

pub struct TapeProcessor {
    enabled: bool,

    // Hysteresis — Jiles-Atherton magnetic saturation. Runs FIRST in the
    // tape chain so the nonlinear character lands before any linear stages.
    // Matches CHOWTape's record-head-then-playback signal flow.
    hysteresis_l: Hysteresis,
    hysteresis_r: Hysteresis,

    // Wow + flutter — sample-by-sample pitch modulation via delay lines.
    // Runs after the saturator and before the loss filter / head bump
    // (playback-side wobble).
    wow_flutter: WowFlutter,

    // Loss filter — physical HF rolloff (head gap, coating thickness,
    // tape-to-head spacing). Independent state per channel.
    loss_l: LossFilter,
    loss_r: LossFilter,

    // Head bump — peaking biquad per channel.
    head_bump_l: Biquad,
    head_bump_r: Biquad,

    // Chew — random-interval dropouts. Defaults off (TC-250 was clean).
    chew: Chew,

    // Hiss — mono white noise, low-passed, summed into both channels.
    noise_osc: Oscillator,
    hiss_lpf: Biquad,
    hiss_amount: f32,

    // Bus compressor — gentle "tape-glue" applied last. Per-channel mono
    // instances (`infinitedsp::Compressor` is FrameProcessor<Mono>).
    comp_enabled: bool,
    comp_l: Compressor,
    comp_r: Compressor,

    // Scratch buffers (allocated lazily, resized per block).
    l_buf: Vec<f32>,
    r_buf: Vec<f32>,
    hiss_buf: Vec<f32>,
}

impl TapeProcessor {
    pub fn new(sample_rate: f32) -> Self {
        let make_head_bump = || {
            let mut b = Biquad::new(
                FilterType::Peaking,
                AudioParam::hz(DEFAULT_BUMP_HZ),
                AudioParam::linear(DEFAULT_BUMP_Q),
            );
            b.set_gain(AudioParam::linear(DEFAULT_BUMP_GAIN_DB));
            b.set_sample_rate(sample_rate);
            b
        };

        let mut hiss_lpf = Biquad::new(
            FilterType::LowPass,
            AudioParam::hz(DEFAULT_HISS_LPF_HZ),
            AudioParam::linear(0.707),
        );
        hiss_lpf.set_sample_rate(sample_rate);

        // WhiteNoise variant of Oscillator ignores the frequency parameter
        // but the API requires one — any value works.
        let mut noise_osc = Oscillator::new(AudioParam::hz(1.0), Waveform::WhiteNoise);
        noise_osc.set_sample_rate(sample_rate);

        let make_comp = || {
            // Gentle bus compression: -6 dB threshold, 1.8:1 ratio, soft knee.
            // ~10 ms attack / 100 ms release matches "tape glue" feel.
            let mut c = Compressor::new(
                AudioParam::linear(-6.0),
                AudioParam::linear(1.8),
            );
            c.set_attack(AudioParam::linear(10.0));
            c.set_release(AudioParam::linear(100.0));
            c.set_knee(AudioParam::linear(6.0));
            c.set_makeup(AudioParam::linear(1.5));
            c.set_sample_rate(sample_rate);
            c
        };

        let mut tape = Self {
            enabled: true,
            hysteresis_l: Hysteresis::new(sample_rate),
            hysteresis_r: Hysteresis::new(sample_rate),
            wow_flutter: WowFlutter::new(sample_rate),
            loss_l: LossFilter::new(sample_rate),
            loss_r: LossFilter::new(sample_rate),
            head_bump_l: make_head_bump(),
            head_bump_r: make_head_bump(),
            chew: Chew::new(sample_rate),
            noise_osc,
            hiss_lpf,
            hiss_amount: DEFAULT_HISS_AMOUNT,
            comp_enabled: true,
            comp_l: make_comp(),
            comp_r: make_comp(),
            l_buf: Vec::new(),
            r_buf: Vec::new(),
            hiss_buf: Vec::new(),
        };
        // Start with the Sony TC-250 preset — the workspace's "house tape".
        // Use [`preset_sony_tc_250`] explicitly or write a sibling preset to
        // model a different machine.
        tape.preset_sony_tc_250();
        tape
    }

    /// Configure the chain to emulate a **Sony TC-250** reel-to-reel deck
    /// (consumer, late-1960s, 1/4" quarter-track stereo).
    ///
    /// Targets the published specs:
    /// - 7.5 IPS high-quality mode
    /// - Frequency response 50 Hz – 15 kHz ±2 dB
    /// - Wow + flutter < 0.19 % combined
    /// - S/N ratio > 50 dB
    /// - THD < 1 % at 0 dB line output
    /// - 2 permalloy heads
    ///
    /// Approximations:
    /// - Head gap fixed at 10 μm (typical permalloy of the era; no datasheet)
    /// - Tape thickness 30 μm (~1.5 mil consumer 1/4" reel tape — the most
    ///   common stock for this class of machine)
    /// - Head-to-tape spacing 3 μm (slight wear; pristine would be < 1 μm)
    /// - Hysteresis settings chosen for ~1 % THD at normal program level
    pub fn preset_sony_tc_250(&mut self) {
        // Loss filter — physical playback losses (HF rolloff to ~15 kHz).
        self.set_speed_ips(7.5);
        self.set_spacing_um(3.0);
        self.set_thickness_um(30.0);
        self.set_gap_um(10.0);

        // Hysteresis — clean machine, moderate drive, narrow loop (low memory).
        // < 1 % THD at line level means we don't want to crush the signal.
        self.set_hysteresis(0.40, 0.50, 0.30);

        // Wow + flutter — close to the 0.19 % spec at 7.5 IPS.
        // Peak speed deviation = depth_seconds · 2π · rate_hz.
        // wow 0.3 ms · 2π · 0.5 Hz ≈ 0.094 % ; flutter ~0.22 % combined.
        let wf = self.wow_flutter_mut();
        wf.set_wow_rate_hz(0.5);
        wf.set_wow_depth_ms(0.3);
        wf.set_flutter_depth_ms(0.05);

        // Hiss — matches the 50 dB S/N spec (10^(-50/20) ≈ 0.32 % linear).
        self.set_hiss_amount(0.0032);

        // Chew — very slight. Models a well-loved but not abused machine:
        // mild attenuation events spaced ~3-4 seconds apart, ~75 ms long.
        // Power range stays gentle (|x|^1.1..1.3) — audible as a brief dip,
        // not as a click or LPF sweep.
        let c = self.chew_mut();
        c.set_depth(0.1);
        c.set_freq(0.1);
        c.set_variance(0.5);
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Hiss level, linear gain (0..1). Reasonable range ~0.0 to 0.01.
    pub fn set_hiss_amount(&mut self, amount: f32) {
        self.hiss_amount = amount.clamp(0.0, 1.0);
    }

    /// Mutable access to the wow/flutter stage (depth, rate, enable).
    pub fn wow_flutter_mut(&mut self) -> &mut WowFlutter {
        &mut self.wow_flutter
    }

    /// Set hysteresis (saturation) params on both channels. All in [0, 1].
    pub fn set_hysteresis(&mut self, drive: f32, width: f32, sat: f32) {
        self.hysteresis_l.cook(drive, width, sat);
        self.hysteresis_r.cook(drive, width, sat);
    }

    /// Mutable access to the chew (dropout) stage.
    pub fn chew_mut(&mut self) -> &mut Chew {
        &mut self.chew
    }

    /// Enable / disable the bus compressor. Defaults on.
    pub fn set_compressor_enabled(&mut self, en: bool) {
        self.comp_enabled = en;
    }
    pub fn compressor_enabled(&self) -> bool {
        self.comp_enabled
    }
    /// Set bus compressor params on both channels in lockstep.
    pub fn set_compressor(&mut self, threshold_db: f32, ratio: f32, attack_ms: f32, release_ms: f32, makeup_db: f32) {
        for c in [&mut self.comp_l, &mut self.comp_r] {
            c.set_threshold(AudioParam::linear(threshold_db));
            c.set_ratio(AudioParam::linear(ratio));
            c.set_attack(AudioParam::linear(attack_ms));
            c.set_release(AudioParam::linear(release_ms));
            c.set_makeup(AudioParam::linear(makeup_db));
        }
    }

    // Loss-filter setters apply to both channels in lockstep; tape physics
    // doesn't vary per channel, only the audio content does.
    pub fn set_speed_ips(&mut self, ips: f32) {
        self.loss_l.set_speed_ips(ips);
        self.loss_r.set_speed_ips(ips);
    }
    pub fn set_spacing_um(&mut self, um: f32) {
        self.loss_l.set_spacing_um(um);
        self.loss_r.set_spacing_um(um);
    }
    pub fn set_thickness_um(&mut self, um: f32) {
        self.loss_l.set_thickness_um(um);
        self.loss_r.set_thickness_um(um);
    }
    pub fn set_gap_um(&mut self, um: f32) {
        self.loss_l.set_gap_um(um);
        self.loss_r.set_gap_um(um);
    }

    /// Apply tape effects to an interleaved-stereo buffer, in place.
    pub fn process(&mut self, output: &mut [f32], sample_index: u64) {
        if !self.enabled {
            return;
        }

        let n_frames = output.len() / 2;
        self.l_buf.resize(n_frames, 0.0);
        self.r_buf.resize(n_frames, 0.0);
        self.hiss_buf.resize(n_frames, 0.0);

        // Deinterleave so the mono stages can run per-channel.
        for (i, frame) in output.chunks_exact(2).enumerate() {
            self.l_buf[i] = frame[0];
            self.r_buf[i] = frame[1];
        }

        // Hysteresis — JA magnetic saturation. Independent state per channel.
        // Runs first so the nonlinearity bites before any linear filtering.
        for s in self.l_buf.iter_mut() {
            *s = self.hysteresis_l.process_sample(*s);
        }
        for s in self.r_buf.iter_mut() {
            *s = self.hysteresis_r.process_sample(*s);
        }

        // Wow + flutter — per-sample shared modulation, independent delay
        // lines per channel. Runs first so downstream stages see the wobbled
        // signal (matches real tape signal flow).
        for i in 0..n_frames {
            let (l, r) = self.wow_flutter.process_sample(self.l_buf[i], self.r_buf[i]);
            self.l_buf[i] = l;
            self.r_buf[i] = r;
        }

        // Loss filter — physical HF rolloff (gap, thickness, spacing).
        self.loss_l.process(&mut self.l_buf);
        self.loss_r.process(&mut self.r_buf);

        // Head bump — independent state per channel.
        self.head_bump_l.process(&mut self.l_buf, sample_index);
        self.head_bump_r.process(&mut self.r_buf, sample_index);

        // Chew — random-interval dropouts (LPF + power-law shaper).
        // Per-sample because the state machine and LPF need it; both
        // channels share the dropout state but have independent LPFs.
        for i in 0..n_frames {
            let mut l = self.l_buf[i];
            let mut r = self.r_buf[i];
            self.chew.process_sample(&mut l, &mut r);
            self.l_buf[i] = l;
            self.r_buf[i] = r;
        }

        // Hiss — generate white noise into a mono scratch buffer, low-pass it,
        // mix into both channels. Same noise feeds both — correlated hiss is
        // what tape sounds like, the signal-path coloration decorrelates.
        self.noise_osc.process(&mut self.hiss_buf, sample_index);
        self.hiss_lpf.process(&mut self.hiss_buf, sample_index);

        let hiss_g = self.hiss_amount;
        for i in 0..n_frames {
            let hiss = self.hiss_buf[i] * hiss_g;
            self.l_buf[i] += hiss;
            self.r_buf[i] += hiss;
        }

        // Bus compressor — gentle 1.8:1 glue, last stage before output.
        if self.comp_enabled {
            self.comp_l.process(&mut self.l_buf, sample_index);
            self.comp_r.process(&mut self.r_buf, sample_index);
        }

        // Re-interleave the final stereo buffer into the output slot.
        for (i, frame) in output.chunks_exact_mut(2).enumerate() {
            frame[0] = self.l_buf[i];
            frame[1] = self.r_buf[i];
        }
    }
}
