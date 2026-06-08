#![cfg_attr(not(test), no_std)]

//! Audio + MIDI core. Runs identically on Daisy firmware and macOS host.
//!
//! Audio is interleaved stereo f32, roughly in [-1.0, 1.0]. The internal
//! `Sampler` does linear-interpolation sample-rate conversion so loaded
//! buffers can be at any source rate. Output is post-processed by an
//! `infinitedsp` Reverb with a wet/dry mix.

extern crate alloc;

use alloc::vec::Vec;

// Public so downstream crates (e.g. host bins) can drive time-based effects:
// `FrameProcessor` carries `process`/`set_sample_rate`, `AudioParam` sets the
// delay's time/feedback, and `PingPongDelay` is the delay itself.
pub use infinitedsp_core::core::audio_param::AudioParam;
use infinitedsp_core::effects::dynamics::distortion::{Distortion, DistortionType};
pub use infinitedsp_core::effects::time::ping_pong_delay::PingPongDelay;
use infinitedsp_core::effects::time::reverb::Reverb;
pub use infinitedsp_core::FrameProcessor;

pub mod analog_bass_drum;
pub mod bass;
pub mod bloom;
pub mod chord;
pub mod fm_stab;
pub mod freeze;
pub mod groove;
pub mod hihat;
pub mod limiter;
pub mod midi;
pub mod midi_map;
pub mod pain_voice;
pub mod sequencer;
pub mod spectre_filter;
pub mod svf;
pub mod tape;
pub mod timeline;

pub use analog_bass_drum::AnalogBassDrum;
pub use bass::{BassPatch, RumbleBass};
pub use fm_stab::{FmPatch, FmStab, Shaper};
pub use groove::{GrooveEvent, Macro, Track};
pub use hihat::HiHat;
pub use midi::{MidiByteParser, MidiMessage};
pub use midi_map::{install_kiosk_bindings, MidiMap, Param};
pub use pain_voice::PainMaterialVoice;
pub use spectre_filter::{
    BandMode, ChannelMode, DynamicBandSettings, DynamicFilter, DynamicFilterSettings, MasterFilter,
    MasterFilterModel, MasterFilterSettings,
};
pub use svf::Svf;

pub struct Engine {
    #[allow(dead_code)] // will be used once we add synth voices alongside the sampler
    sample_rate: f32,
    sampler: Sampler,
    kick: AnalogBassDrum,
    /// Soft-clip saturation on the kick path — the "warmth + glue" that
    /// turns a raw 808-model output into a techno-kick character. Configured
    /// as fully wet (it's the drum's voice, not a wet/dry effect).
    kick_dist: Distortion,
    /// Mono scratch buffer for kick samples between synthesis and distortion.
    kick_buf: Vec<f32>,
    /// Two hi-hat voices — closed (short decay) and open (long decay) —
    /// so they can overlap if the sequencer fires one while the other rings.
    hihat_closed: HiHat,
    hihat_open: HiHat,
    /// Mono scratch buffer for the summed hi-hat output.
    hihat_buf: Vec<f32>,
    /// Master gain on the hi-hat bus (the raw HiHat output is bright/hot).
    hihat_gain: f32,
    /// Polyphonic FM "stab" synth — DX7-flavoured chord hits driven by the
    /// sequencer's stab lane (and by live MIDI note-on of non-kick notes).
    stabs: fm_stab::FmStab,
    /// Mono scratch buffer for the summed stab output.
    stab_buf: Vec<f32>,
    /// Stereo send buffer feeding the stab ping-pong delay. The dry stab is
    /// panned hard left into this buffer so the cross-fed echoes bounce L↔R
    /// (the classic Cybotron / Detroit-dub stab delay).
    stab_send: Vec<f32>,
    /// Ping-pong delay on the stab bus. Runs wet-only (`mix = 1.0`); its
    /// output is the bouncing echoes alone, which we fold back into the master
    /// *before* the reverb so each repeat trails off into the room.
    stab_delay: PingPongDelay,
    /// How much of the delay's wet echoes to fold into the master (0..1).
    stab_delay_wet: f32,
    /// Monophonic subtractive "rumble bass" — detuned saws + sub sine through a
    /// resonant LPF, gate-driven by the sequencer's bass lane. Rendered dry and
    /// centred (no ping-pong / minimal reverb) so the low end stays tight.
    bass: bass::RumbleBass,
    /// Mono scratch buffer for the bass output.
    bass_buf: Vec<f32>,
    /// Gain multiplier applied to the kick's output. Set per-trigger from
    /// MIDI velocity (0..1), so soft pad hits play a quieter kick.
    kick_velocity: f32,
    /// Master gain on the kick bus. Separate from per-hit velocity so hardware
    /// macros can mix the kick lane without changing pattern accents.
    kick_gain: f32,
    /// MIDI note number that triggers the kick. Defaults to 60 (C4) — pads
    /// in "note mode" on most controllers. Change via [`Engine::set_kick_trigger_note`].
    kick_trigger_note: u8,
    midi_map: MidiMap,
    sequencer: sequencer::Sequencer,
    reverb: Reverb,
    /// Resonant "bloom" bank — rings the pre-tape master into a fixed D Lydian
    /// chord, scaled by a "proximity" amount. Stands in for the exhibit's ToF
    /// "approach pulls clarity + tone out of the rain" gesture.
    bloom: bloom::BloomBank,
    /// Standalone Spectre-inspired master filter insert. Default settings are
    /// bypassed, so it is silent in old patches until explicitly enabled.
    spectre_filter: spectre_filter::MasterFilter,
    spectre_filter_settings: spectre_filter::MasterFilterSettings,
    /// Standalone Spectre-inspired dynamic band rack. All bands default to
    /// bypassed; groove macros can open the first band as a performance filter.
    spectre_dynamic_filter: spectre_filter::DynamicFilter,
    spectre_dynamic_settings: spectre_filter::DynamicFilterSettings,
    tape: tape::TapeProcessor,
    /// Master "freeze" — parallel-send grain hold; produces a looped grain of
    /// the post-everything master, run through `glitch_tape` and summed *under*
    /// the live master. Audio mirror of the visualizer's frame-freeze. Driven
    /// by [`Engine::set_freeze`]; the exhibit transport for it is unconnected.
    freeze: freeze::Freeze,
    /// Stripped failure-tape (wow/flutter + chew) for the freeze ghost. Run
    /// only while the freeze is active.
    glitch_tape: freeze::GlitchTape,
    /// Scratch for the freeze ghost send (stereo interleaved).
    freeze_send: Vec<f32>,
    /// Master peak limiter on the summed (master + ghost) output.
    limiter: limiter::Limiter,
    /// Holds the dry sampler output across the reverb call so we can mix wet+dry.
    dry_scratch: Vec<f32>,
    /// Global sample index, fed to FrameProcessor::process for time-aware effects.
    sample_index: u64,
    /// 0.0 = fully dry, 1.0 = fully wet.
    reverb_wet: f32,
    /// When `false`, the step sequencer is not advanced and fires no kick /
    /// hat / stab triggers — the sampler, reverb, tape and bloom still run.
    /// Lets a host audition the track + processing without the drum pattern.
    sequencer_enabled: bool,
    /// Current hardware-selected lane. The DSP core stores it so host and
    /// firmware surfaces can share selection semantics without UI state.
    selected_track: Track,
}

impl Engine {
    pub fn new(sample_rate: f32) -> Self {
        let mut reverb = Reverb::new();
        reverb.set_sample_rate(sample_rate);

        // Drive 2.0 ≈ tanh saturation that kicks in on peaks; subtle warmth,
        // not "obviously distorted". Bump drive on `kick_dist_mut()` for grit.
        let mut kick_dist = Distortion::new(
            AudioParam::linear(2.0),
            AudioParam::linear(1.0),
            DistortionType::SoftClip,
        );
        kick_dist.set_sample_rate(sample_rate);

        // Cybotron-style ping-pong on the stab bus: ~dotted-8th echoes
        // (0.375 s) that bounce L↔R with enough feedback for a few audible
        // repeats. `mix = 1.0` so the processed buffer carries *only* the wet
        // echoes — we add them on top of the dry stab in the master.
        //
        // NOTE: the delay line allocates 2 × max_delay_s × sample_rate f32 from
        // the global allocator. On the Daisy that's far larger than the current
        // firmware heap — like the reverb, these buffers belong in SDRAM once
        // the firmware audio path is wired. On the host it's free.
        let mut stab_delay = PingPongDelay::new(
            1.0,                        // max delay buffer, seconds
            AudioParam::seconds(0.375), // dotted-8th-ish at techno tempo
            AudioParam::linear(0.45),   // feedback → a few repeats
            AudioParam::linear(1.0),    // mix = wet-only output
        );
        stab_delay.set_sample_rate(sample_rate);

        Self {
            sample_rate,
            sampler: Sampler::new(),
            kick: AnalogBassDrum::new(sample_rate),
            kick_dist,
            kick_buf: Vec::new(),
            // Closed hi-hat — 909-style: bright, metallic, slight body.
            hihat_closed: {
                let mut h = HiHat::new(sample_rate);
                h.set_freq(4000.0); // spreads the 6-osc metallic stack to 4-9 kHz
                h.set_decay(0.6); // a touch of body (was 0.2 = clicky)
                h.set_accent(0.95);
                h.set_tone(0.85); // BPF/HPF cutoff ~6-7 kHz where 909 sits
                h.set_noisiness(0.6); // less broadband noise — main character was "shaker"
                h
            },
            // Open hi-hat — same colour, longer decay, slightly softer accent.
            hihat_open: {
                let mut h = HiHat::new(sample_rate);
                h.set_freq(4000.0);
                h.set_decay(0.95); // long ring (was 0.7)
                h.set_accent(0.8);
                h.set_tone(0.85);
                h.set_noisiness(0.6);
                h
            },
            hihat_buf: Vec::new(),
            hihat_gain: 0.5,
            stabs: {
                // Default to the abrasive industrial patch — inharmonic FM,
                // operator feedback, hard-clipped. Swap with `stabs_mut()
                // .load_patch(FmPatch::default())` for the clean DX stab.
                let mut s = fm_stab::FmStab::new(sample_rate);
                // s.load_patch(fm_stab::FmPatch::industrial());
                // temp: patch to default to see how it sounds
                s.load_patch(fm_stab::FmPatch::default());
                s
            },
            stab_buf: Vec::new(),
            stab_send: Vec::new(),
            stab_delay,
            stab_delay_wet: 0.5,
            bass: bass::RumbleBass::new(sample_rate),
            bass_buf: Vec::new(),
            kick_velocity: 1.0,
            kick_gain: 1.0,
            kick_trigger_note: 60,
            midi_map: MidiMap::new(),
            sequencer: sequencer::Sequencer::new(sample_rate),
            reverb,
            bloom: bloom::BloomBank::new(sample_rate),
            spectre_filter: spectre_filter::MasterFilter::new(sample_rate),
            spectre_filter_settings: spectre_filter::MasterFilterSettings::default(),
            spectre_dynamic_filter: spectre_filter::DynamicFilter::new(sample_rate),
            spectre_dynamic_settings: spectre_filter::DynamicFilterSettings::default(),
            tape: tape::TapeProcessor::new(sample_rate),
            freeze: freeze::Freeze::new(sample_rate),
            glitch_tape: freeze::GlitchTape::new(sample_rate),
            freeze_send: Vec::new(),
            limiter: limiter::Limiter::new(sample_rate),
            dry_scratch: Vec::new(),
            sample_index: 0,
            // A wash of room by default — the cold, spacious Cybotron air. Dial
            // with `set_reverb_wet` / the ReverbWet CC.
            reverb_wet: 0.18,
            sequencer_enabled: true,
            selected_track: Track::Kick,
        }
    }

    /// Load a stereo-interleaved f32 sample. `src_sample_rate` is the rate at
    /// which the buffer was recorded; playback resamples on the fly. `buf` must
    /// outlive the engine (typically `Box::leak` on host, `static` on embedded).
    pub fn load_sample(&mut self, buf: &'static [f32], src_sample_rate: f32) {
        debug_assert_eq!(buf.len() % 2, 0, "sample buffer must be interleaved stereo");
        // Compute the rate ratio in f64 so the sampler's long-running
        // position accumulator stays precise (see Sampler::position docs).
        self.sampler
            .load(buf, src_sample_rate as f64 / self.sample_rate as f64);
    }

    pub fn play(&mut self, looping: bool) {
        self.sampler.play(looping);
    }

    pub fn stop(&mut self) {
        self.sampler.stop();
    }

    pub fn set_reverb_wet(&mut self, wet: f32) {
        self.reverb_wet = wet.clamp(0.0, 1.0);
    }

    /// Strike the analog bass drum on the next process() call.
    /// `velocity` (0..1) scales the kick's output gain — passed straight
    /// from MIDI velocity (`velocity_byte as f32 / 127.0`).
    pub fn trigger_kick(&mut self, velocity: f32) {
        self.kick_velocity = velocity.clamp(0.0, 1.0);
        self.kick.trig();
    }

    /// Change which MIDI note number triggers the kick. Default is 60 (C4).
    pub fn set_kick_trigger_note(&mut self, note: u8) {
        self.kick_trigger_note = note;
    }

    /// Mutable access to the kick drum for tweaking freq/decay/tone/etc.
    pub fn kick_mut(&mut self) -> &mut AnalogBassDrum {
        &mut self.kick
    }

    /// Mutable access to the kick-bus distortion (drive, mix, type).
    pub fn kick_dist_mut(&mut self) -> &mut Distortion {
        &mut self.kick_dist
    }

    /// Mutable access to the FM stab synth (patch, gain, manual triggering).
    pub fn stabs_mut(&mut self) -> &mut fm_stab::FmStab {
        &mut self.stabs
    }

    /// Mutable access to the stab ping-pong delay (delay time / feedback / mix).
    pub fn stab_delay_mut(&mut self) -> &mut PingPongDelay {
        &mut self.stab_delay
    }

    /// How much of the stab delay's wet echoes is folded into the master (0..1).
    pub fn set_stab_delay_wet(&mut self, wet: f32) {
        self.stab_delay_wet = wet.clamp(0.0, 1.0);
    }
    pub fn stab_delay_wet(&self) -> f32 {
        self.stab_delay_wet
    }

    /// Mutable access to the reverb (room size, damping).
    pub fn reverb_mut(&mut self) -> &mut Reverb {
        &mut self.reverb
    }

    /// Mutable access to the rumble-bass voice (cutoff, resonance, patch, gain).
    pub fn bass_mut(&mut self) -> &mut bass::RumbleBass {
        &mut self.bass
    }

    /// Configure MIDI CC bindings. Call from the host at startup.
    pub fn midi_map_mut(&mut self) -> &mut MidiMap {
        &mut self.midi_map
    }

    /// Read-only view of the MIDI map (for debug printing, etc.).
    pub fn midi_map(&self) -> &MidiMap {
        &self.midi_map
    }

    /// Mutable access to the step sequencer (set tempo, pattern, etc.).
    pub fn sequencer_mut(&mut self) -> &mut sequencer::Sequencer {
        &mut self.sequencer
    }

    /// Read-only view of the sequencer (current step, time, etc.).
    pub fn sequencer(&self) -> &sequencer::Sequencer {
        &self.sequencer
    }

    /// Mutable access to the tape processor (enable/disable, hiss level, etc.).
    pub fn tape_mut(&mut self) -> &mut tape::TapeProcessor {
        &mut self.tape
    }

    /// Set the resonant-bloom "proximity" amount (0 = far/silent, 1 = near/full
    /// bloom). The bank rings the pre-tape master into a fixed D Lydian chord.
    pub fn set_bloom_amount(&mut self, amount: f32) {
        self.bloom.set_amount(amount);
    }
    pub fn bloom_amount(&self) -> f32 {
        self.bloom.amount()
    }

    pub fn set_spectre_filter(&mut self, settings: MasterFilterSettings) {
        self.spectre_filter_settings = settings;
    }
    pub fn spectre_filter_settings(&self) -> MasterFilterSettings {
        self.spectre_filter_settings
    }
    pub fn spectre_filter_mut(&mut self) -> &mut MasterFilter {
        &mut self.spectre_filter
    }
    pub fn set_spectre_dynamic_filter(&mut self, settings: DynamicFilterSettings) {
        self.spectre_dynamic_settings = settings;
    }
    pub fn spectre_dynamic_filter_settings(&self) -> DynamicFilterSettings {
        self.spectre_dynamic_settings
    }
    pub fn spectre_dynamic_filter_mut(&mut self) -> &mut DynamicFilter {
        &mut self.spectre_dynamic_filter
    }

    /// Set the master freeze wet/dry amount (0 = passthrough, 1 = held grain).
    /// Crossing above 0 latches the current grain and loops it.
    pub fn set_freeze(&mut self, amount: f32) {
        self.freeze.set_amount(amount);
    }
    pub fn freeze_amount(&self) -> f32 {
        self.freeze.amount()
    }

    /// Enable/disable the step sequencer. When disabled, no kick/hat/stab
    /// triggers fire and the sequencer clock is frozen; the sampler, reverb,
    /// tape and bloom keep running so the track + processing can be auditioned
    /// without the drum pattern.
    pub fn set_sequencer_enabled(&mut self, enabled: bool) {
        self.sequencer_enabled = enabled;
    }
    pub fn sequencer_enabled(&self) -> bool {
        self.sequencer_enabled
    }

    pub fn set_kick_gain(&mut self, g: f32) {
        self.kick_gain = g.max(0.0);
    }
    pub fn kick_gain(&self) -> f32 {
        self.kick_gain
    }

    /// Master gain on the hi-hat bus (default 0.6).
    pub fn set_hihat_gain(&mut self, g: f32) {
        self.hihat_gain = g.max(0.0);
    }
    pub fn hihat_gain(&self) -> f32 {
        self.hihat_gain
    }

    /// Mutable access to the closed hi-hat voice (decay, tone, etc.).
    pub fn hihat_closed_mut(&mut self) -> &mut HiHat {
        &mut self.hihat_closed
    }
    /// Mutable access to the open hi-hat voice.
    pub fn hihat_open_mut(&mut self) -> &mut HiHat {
        &mut self.hihat_open
    }

    pub fn selected_track(&self) -> Track {
        self.selected_track
    }

    /// Apply one decoded groovebox control event. This is intentionally small
    /// and bounded so any host can translate its hardware/UI protocol into the
    /// same action type before touching the realtime engine.
    pub fn handle_groove_event(&mut self, evt: GrooveEvent) {
        match evt {
            GrooveEvent::TransportPlay(playing) => self.set_sequencer_enabled(playing),
            GrooveEvent::TransportReset => self.sequencer.reset(),
            GrooveEvent::SelectTrack(track) => self.selected_track = track,
            GrooveEvent::ToggleStep { track, step } => self.toggle_step(track, step as usize),
            GrooveEvent::SetStepVelocity {
                track,
                step,
                velocity,
            } => self.set_step_velocity(track, step as usize, velocity),
            GrooveEvent::SetMacro { macro_id, value } => {
                if let Some(m) = Macro::from_id(macro_id) {
                    self.apply_macro(m, value);
                }
            }
            GrooveEvent::Pad { note, velocity } => self.trigger_pad(note, velocity),
        }
    }

    fn toggle_step(&mut self, track: Track, step: usize) {
        if track == Track::Bass {
            let next = match self.sequencer.bass_step(step) {
                Some(sequencer::BassCell::Rest) => sequencer::BassCell::Strike(1.0),
                Some(_) => sequencer::BassCell::Rest,
                None => return,
            };
            self.sequencer.set_bass_step(step, next);
            return;
        }

        let Some(voice) = track.sequencer_voice() else {
            return;
        };
        let Some(current) = self.sequencer.step_velocity(voice, step) else {
            return;
        };
        self.sequencer
            .set_step_velocity(voice, step, if current > 0.0 { 0.0 } else { 1.0 });
    }

    fn set_step_velocity(&mut self, track: Track, step: usize, velocity: f32) {
        let v = velocity.clamp(0.0, 1.0);
        if track == Track::Bass {
            self.sequencer.set_bass_step(
                step,
                if v > 0.0 {
                    sequencer::BassCell::Strike(v)
                } else {
                    sequencer::BassCell::Rest
                },
            );
            return;
        }

        if let Some(voice) = track.sequencer_voice() {
            self.sequencer.set_step_velocity(voice, step, v);
        }
    }

    fn apply_macro(&mut self, m: Macro, value: f32) {
        let v = value.clamp(0.0, 1.0);
        match m {
            Macro::Damage => {
                self.tape.set_failure(v);
                self.kick_dist.set_drive(AudioParam::linear(1.0 + v * 5.0));
                self.freeze
                    .set_amount(if v > 0.85 { (v - 0.85) / 0.15 } else { 0.0 });
            }
            Macro::Space => {
                self.reverb_wet = 0.05 + v * 0.45;
                self.stab_delay_wet = v;
                self.stab_delay
                    .set_feedback(AudioParam::linear(0.2 + v * 0.55));
                self.bloom.set_amount(v);
            }
            Macro::Tone => {
                self.kick.set_tone(v);
                self.hihat_closed.set_tone(v);
                self.hihat_open.set_tone(v);
                self.stabs.set_index(0.2 + v * 3.8);
                self.bass.set_cutoff(80.0 + v * 3920.0);
                self.spectre_filter_settings = MasterFilterSettings {
                    model: MasterFilterModel::SemMorph,
                    cutoff_hz: 120.0 + v * 15_000.0,
                    resonance: 0.15 + v * 0.45,
                    drive: v * 0.35,
                    morph: v,
                    mix: v * 0.6,
                };
            }
            Macro::KickLevel => self.kick_gain = v * 1.5,
            Macro::HatLevel => self.hihat_gain = v,
            Macro::StabLevel => self.stabs.set_gain(v),
            Macro::BassLevel => self.bass.set_gain(v),
            Macro::FilterCutoff => {
                let mut settings = self.spectre_dynamic_settings;
                settings.bands[0].enabled = v > 0.0;
                settings.bands[0].mode = BandMode::Bell;
                settings.bands[0].channel_mode = ChannelMode::Stereo;
                settings.bands[0].frequency_hz = 80.0 * libm::powf(100.0, v);
                settings.bands[0].gain_db = 0.0;
                settings.bands[0].q = settings.bands[0].q.clamp(0.4, 8.0);
                settings.bands[0].env_attack_ms = 6.0;
                settings.bands[0].env_release_ms = 180.0;
                self.spectre_dynamic_settings = settings;
            }
            Macro::FilterResonance => {
                let mut settings = self.spectre_dynamic_settings;
                settings.bands[0].enabled = true;
                settings.bands[0].mode = BandMode::Bell;
                settings.bands[0].channel_mode = ChannelMode::Stereo;
                settings.bands[0].q = 0.4 + v * 7.6;
                self.spectre_dynamic_settings = settings;
            }
            Macro::FilterMotion => {
                let mut settings = self.spectre_dynamic_settings;
                settings.bands[0].enabled = v > 0.0;
                settings.bands[0].mode = BandMode::Bell;
                settings.bands[0].channel_mode = ChannelMode::Stereo;
                settings.bands[0].dynamic_db = -18.0 + v * 36.0;
                settings.bands[0].sweep_octaves = v * 2.0;
                settings.bands[0].env_sensitivity = 0.35 + v * 2.65;
                settings.bands[0].env_attack_ms = 4.0 + (1.0 - v) * 20.0;
                settings.bands[0].env_release_ms = 80.0 + v * 360.0;
                self.spectre_dynamic_settings = settings;
            }
        }
    }

    fn trigger_pad(&mut self, note: u8, velocity: f32) {
        let v = velocity.clamp(0.0, 1.0);
        if v <= 0.0 {
            return;
        }

        match note {
            // GM-ish drum note plus whichever note the engine uses for kick.
            n if n == 36 || n == self.kick_trigger_note => self.trigger_kick(v),
            42 | 44 => self.hihat_closed.trig(),
            46 => self.hihat_open.trig(),
            _ => self.stabs.note_on(note, v),
        }
    }

    /// Dispatch an inbound MIDI message. Note-on of the kick trigger note
    /// fires the kick; any other note-on plays an FM stab voice. CC messages
    /// are routed through the [`MidiMap`].
    pub fn handle_midi(&mut self, msg: MidiMessage) {
        match msg {
            MidiMessage::ControlChange { cc, value, .. } => {
                if let Some((param, mapped)) = self.midi_map.map_cc(cc, value) {
                    self.apply_param(param, mapped);
                }
            }
            MidiMessage::NoteOn { note, velocity, .. } if velocity > 0 => {
                if note == self.kick_trigger_note {
                    self.trigger_kick(velocity as f32 / 127.0);
                } else {
                    // Playable FM stab — auditions the synth from a controller.
                    self.stabs.note_on(note, velocity as f32 / 127.0);
                }
            }
            _ => {}
        }
    }

    pub fn apply_param(&mut self, param: Param, value: f32) {
        match param {
            Param::KickFreq => self.kick.set_freq(value),
            Param::KickAccent => self.kick.set_accent(value),
            Param::KickDecay => self.kick.set_decay(value),
            Param::KickTone => self.kick.set_tone(value),
            Param::KickAttackFm => self.kick.set_attack_fm_amount(value),
            Param::KickSelfFm => self.kick.set_self_fm_amount(value),
            Param::KickDistDrive => self.kick_dist.set_drive(AudioParam::linear(value)),
            Param::ReverbWet => self.reverb_wet = value.clamp(0.0, 1.0),
            Param::StabGain => self.stabs.set_gain(value),
            Param::StabIndex => self.stabs.set_index(value),
            Param::StabDecay => self.stabs.set_decay(value),
            Param::StabModRatio => self.stabs.set_mod_ratio(value),
            Param::StabFeedback => self.stabs.set_feedback(value),
            Param::StabDrive => self.stabs.set_drive(value),
            Param::TapeFailure => self.tape.set_failure(value),
            Param::StabDelayWet => self.stab_delay_wet = value.clamp(0.0, 1.0),
            Param::StabDelayFeedback => self
                .stab_delay
                .set_feedback(AudioParam::linear(value.clamp(0.0, 0.95))),
            Param::StabDelayTime => self
                .stab_delay
                .set_delay_time(AudioParam::seconds(value.max(0.0))),
            Param::BassCutoff => self.bass.set_cutoff(value),
            Param::BassRes => self.bass.set_resonance(value),
            Param::BassEnvMod => self.bass.set_env_mod(value),
            Param::BassGain => self.bass.set_gain(value),
            Param::Freeze => self.freeze.set_amount(value),
        }
    }

    /// Render one block. `_input` is reserved for future passthrough/sidechain;
    /// `output` (interleaved stereo) is fully overwritten.
    pub fn process(&mut self, _input: &[f32], output: &mut [f32]) {
        // 1. Sampler fills output (cleared first).
        for s in output.iter_mut() {
            *s = 0.0;
        }
        self.sampler.mix_into(output);

        // 2. Render kick, hi-hats and stabs per sample driven by the sequencer.
        //    StepEvent carries the kick velocity (None = no kick this sample,
        //    Some(v) = trigger at velocity v), the closed/open hat flags, and
        //    an optional stab chord. Kick goes through its own distortion
        //    stage; hats and stabs render into mono scratch and mix in. The
        //    stab is also panned into `stab_send` (hard left) to feed the
        //    ping-pong delay.
        let n_frames = output.len() / 2;
        self.kick_buf.resize(n_frames, 0.0);
        self.hihat_buf.resize(n_frames, 0.0);
        self.stab_buf.resize(n_frames, 0.0);
        self.stab_send.resize(output.len(), 0.0);
        self.bass_buf.resize(n_frames, 0.0);
        for i in 0..n_frames {
            // When disabled, freeze the sequencer clock and fire nothing; the
            // voices still tick (with no trigger) so any ringing tail decays.
            let evt = if self.sequencer_enabled {
                self.sequencer.advance()
            } else {
                sequencer::StepEvent::default()
            };
            let kick_trig = if let Some(v) = evt.kick_velocity {
                self.kick_velocity = v;
                true
            } else {
                false
            };
            self.kick_buf[i] = self.kick.process(kick_trig);
            self.hihat_buf[i] =
                self.hihat_closed.process(evt.closed_hat) + self.hihat_open.process(evt.open_hat);

            if let Some(hit) = evt.stab {
                self.stabs
                    .play_chord_toned(hit.chord.notes(), hit.velocity, hit.tone);
            }
            let st = self.stabs.tick();
            self.stab_buf[i] = st;
            // Feed the delay send on the left only; the cross-feedback makes
            // the echoes ping-pong across the stereo field.
            self.stab_send[2 * i] = st;
            self.stab_send[2 * i + 1] = 0.0;

            // Rumble bass — gate events drive the mono voice's envelope.
            match evt.bass {
                sequencer::BassEvent::NoteOn { note, vel } => self.bass.note_on(note, vel),
                sequencer::BassEvent::NoteOff => self.bass.note_off(),
                sequencer::BassEvent::None => {}
            }
            self.bass_buf[i] = self.bass.tick();
        }
        self.kick_dist
            .process(&mut self.kick_buf, self.sample_index);
        // Velocity scales the post-distortion kick — softer hits = quieter
        // *and* slightly less driven character relative to the dry path.
        let vel = self.kick_velocity;
        let hh_gain = self.hihat_gain;
        for (((out_frame, &k), &h), &st) in output
            .chunks_exact_mut(2)
            .zip(self.kick_buf.iter())
            .zip(self.hihat_buf.iter())
            .zip(self.stab_buf.iter())
        {
            let kick_mix = k * vel * self.kick_gain;
            let hat_mix = h * hh_gain;
            out_frame[0] += kick_mix + hat_mix + st;
            out_frame[1] += kick_mix + hat_mix + st;
        }

        // 2b. Ping-pong delay on the stab send (wet-only) — fold the bouncing
        //     echoes into the master *before* the reverb so each repeat trails
        //     off into the room. Cybotron in a box.
        self.stab_delay
            .process(&mut self.stab_send, self.sample_index);
        let delay_wet = self.stab_delay_wet;
        for (out, &w) in output.iter_mut().zip(self.stab_send.iter()) {
            *out += w * delay_wet;
        }

        // 3. Stash the dry signal so we can blend wet+dry after the reverb runs.
        self.dry_scratch.resize(output.len(), 0.0);
        self.dry_scratch.copy_from_slice(output);

        // 3. Reverb replaces output with its fully-wet signal, in place.
        self.reverb.process(output, self.sample_index);

        // 4. Blend.
        let dry_gain = 1.0 - self.reverb_wet;
        let wet_gain = self.reverb_wet;
        for (out, &dry) in output.iter_mut().zip(self.dry_scratch.iter()) {
            *out = dry * dry_gain + *out * wet_gain;
        }

        // 5b. Resonant "bloom" bank — taps the pre-tape master, rings the
        //     program material (rain / pads / street noise) into a fixed
        //     D Lydian chord scaled by the "proximity" amount, and folds it
        //     back here so the bloom shares the same tape character as the
        //     rest of the mix. Skipped entirely when the amount is zero.
        if self.bloom.amount() > 0.0 {
            for frame in output.chunks_exact_mut(2) {
                let mono = 0.5 * (frame[0] + frame[1]);
                let b = self.bloom.tick(mono);
                frame[0] += b;
                frame[1] += b;
            }
        }

        // 5c. Rumble bass — summed dry and centred *after* reverb + bloom so
        //     the sub stays tight and uncoloured, but *before* tape so it
        //     shares the master's tape glue. Mono → both channels.
        for (frame, &b) in output.chunks_exact_mut(2).zip(self.bass_buf.iter()) {
            frame[0] += b;
            frame[1] += b;
        }

        // 5d. Standalone Spectre-inspired dynamic rack + master filter. These
        // zero-allocation inserts carry the first audio-first import from
        // Spectre-Filter while staying bypassed in default patches.
        self.spectre_dynamic_filter
            .process_buffer(output, &self.spectre_dynamic_settings);
        self.spectre_filter
            .process_buffer(output, self.spectre_filter_settings);

        // 6. Tape emulation — final stage on the master bus. Currently
        //    Phase 1 (head bump + hiss); wow/flutter, loss filter, and
        //    hysteresis chain in here as they land. See TAPE_SIMULATION.md.
        self.tape.process(output, self.sample_index);

        // 7. Parallel freeze ghost. The live master above plays untouched; the
        //    freeze holds a grain of it and (only while active) runs it through
        //    the stripped failure-tape, summed *under* the master at a fixed
        //    trim. The producer always runs (cheap: records the ring or reads
        //    the loop); the glitch-tape + sum are gated on `active()`.
        self.freeze_send.resize(output.len(), 0.0);
        self.freeze.process(output, &mut self.freeze_send);
        if self.freeze.active() {
            self.glitch_tape.process(&mut self.freeze_send);
            for (o, &g) in output.iter_mut().zip(self.freeze_send.iter()) {
                *o += g * freeze::FREEZE_RETURN_GAIN;
            }
        }

        // 8. Master limiter — runs always; transparent for the dry master,
        //    brick-walls the summed (master + ghost) so the ghost can't clip or
        //    push the level past the original. The sum (step 7) is before this.
        self.limiter.process(output);

        self.sample_index += (output.len() / 2) as u64;
    }
}

struct Sampler {
    buf: Option<&'static [f32]>,
    frames: usize,
    /// Fractional read position in frames. **f64** because long samples
    /// (e.g. 19-minute MP3 at 44.1 kHz ≈ 50 M frames) push past f32's
    /// 24-bit mantissa: ULP grows above the per-sample `step` and the
    /// counter stops advancing, silently freezing playback. f64's ULP
    /// stays below 1e-6 even at billions of frames.
    position: f64,
    /// Frames advanced per output frame. = src_rate / engine_rate.
    /// Kept in f64 so `position += step` does its arithmetic at f64
    /// precision; the source value is always ~1.0 in magnitude so f32
    /// would suffice, but mixing types just risks regressions.
    step: f64,
    playing: bool,
    looping: bool,
    gain: f32,
}

impl Sampler {
    const fn new() -> Self {
        Self {
            buf: None,
            frames: 0,
            position: 0.0,
            step: 1.0,
            playing: false,
            looping: false,
            gain: 0.7,
        }
    }

    fn load(&mut self, buf: &'static [f32], step: f64) {
        self.buf = Some(buf);
        self.frames = buf.len() / 2;
        self.position = 0.0;
        self.step = step;
    }

    fn play(&mut self, looping: bool) {
        self.position = 0.0;
        self.playing = true;
        self.looping = looping;
    }

    fn stop(&mut self) {
        self.playing = false;
    }

    fn mix_into(&mut self, output: &mut [f32]) {
        let Some(buf) = self.buf else { return };
        if !self.playing || self.frames < 2 {
            return;
        }

        for out_frame in output.chunks_exact_mut(2) {
            while self.position as usize >= self.frames {
                if self.looping {
                    self.position -= self.frames as f64;
                } else {
                    self.playing = false;
                    return;
                }
            }

            let pos_int = self.position as usize;
            let frac = (self.position - pos_int as f64) as f32;
            let i0 = pos_int * 2;
            // Wrap interpolation neighbour to the start when looping so the
            // loop seam doesn't click.
            let i1 = if pos_int + 1 < self.frames {
                (pos_int + 1) * 2
            } else if self.looping {
                0
            } else {
                i0
            };

            let l = buf[i0] + (buf[i1] - buf[i0]) * frac;
            let r = buf[i0 + 1] + (buf[i1 + 1] - buf[i0 + 1]) * frac;

            out_frame[0] += l * self.gain;
            out_frame[1] += r * self.gain;

            self.position += self.step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groove_event_toggles_drum_steps() {
        let mut engine = Engine::new(48_000.0);

        engine
            .sequencer_mut()
            .set_step_velocity(sequencer::Voice::Kick, 3, 0.0);
        engine.handle_groove_event(GrooveEvent::ToggleStep {
            track: Track::Kick,
            step: 3,
        });
        assert_eq!(
            engine.sequencer().step_velocity(sequencer::Voice::Kick, 3),
            Some(1.0)
        );

        engine.handle_groove_event(GrooveEvent::ToggleStep {
            track: Track::Kick,
            step: 3,
        });
        assert_eq!(
            engine.sequencer().step_velocity(sequencer::Voice::Kick, 3),
            Some(0.0)
        );
    }

    #[test]
    fn groove_event_sets_bass_and_macros() {
        let mut engine = Engine::new(48_000.0);

        engine.handle_groove_event(GrooveEvent::SetStepVelocity {
            track: Track::Bass,
            step: 4,
            velocity: 0.6,
        });
        assert_eq!(
            engine.sequencer().bass_step(4),
            Some(sequencer::BassCell::Strike(0.6))
        );

        engine.handle_groove_event(GrooveEvent::SetMacro {
            macro_id: 3,
            value: 0.5,
        });
        assert_eq!(engine.kick_gain(), 0.75);

        engine.handle_groove_event(GrooveEvent::SetMacro {
            macro_id: Macro::FilterMotion.id(),
            value: 1.0,
        });
        let filter = engine.spectre_dynamic_filter_settings();
        assert!(filter.bands[0].enabled);
        assert!(filter.bands[0].dynamic_db > 0.0);
    }

    #[test]
    fn groove_pad_respects_custom_kick_note() {
        let mut engine = Engine::new(48_000.0);
        engine.set_kick_trigger_note(48);
        engine.handle_groove_event(GrooveEvent::Pad {
            note: 48,
            velocity: 1.0,
        });

        let mut out = [0.0_f32; 4096];
        engine.process(&[], &mut out);
        assert!(out.iter().any(|s| s.abs() > 0.0));
    }

    #[test]
    fn spectre_filter_insert_stays_finite_when_enabled() {
        let mut engine = Engine::new(48_000.0);
        engine.set_spectre_filter(MasterFilterSettings {
            model: MasterFilterModel::Diode,
            cutoff_hz: 900.0,
            resonance: 0.85,
            drive: 0.9,
            morph: 0.0,
            mix: 1.0,
        });
        engine.handle_groove_event(GrooveEvent::Pad {
            note: 36,
            velocity: 1.0,
        });

        let mut out = [0.0_f32; 4096];
        engine.process(&[], &mut out);
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn spectre_dynamic_filter_insert_stays_finite_when_enabled() {
        let mut engine = Engine::new(48_000.0);
        let mut settings = DynamicFilterSettings::default();
        settings.bands[0] = DynamicBandSettings {
            enabled: true,
            mode: BandMode::BandPass,
            channel_mode: ChannelMode::Stereo,
            frequency_hz: 650.0,
            gain_db: 6.0,
            q: 1.2,
            dynamic_db: 10.0,
            sweep_octaves: 0.5,
            env_attack_ms: 4.0,
            env_release_ms: 160.0,
            env_sensitivity: 1.4,
        };
        engine.set_spectre_dynamic_filter(settings);
        engine.handle_groove_event(GrooveEvent::Pad {
            note: 36,
            velocity: 1.0,
        });

        let mut out = [0.0_f32; 4096];
        engine.process(&[], &mut out);
        assert!(out.iter().all(|s| s.is_finite()));
    }
}
