#![cfg_attr(not(test), no_std)]

//! Audio + MIDI core. Runs identically on Daisy firmware and macOS host.
//!
//! Audio is interleaved stereo f32, roughly in [-1.0, 1.0]. The internal
//! `Sampler` does linear-interpolation sample-rate conversion so loaded
//! buffers can be at any source rate. Output is post-processed by an
//! `infinitedsp` Reverb with a wet/dry mix.

extern crate alloc;

use alloc::vec::Vec;

use infinitedsp_core::FrameProcessor;
use infinitedsp_core::core::audio_param::AudioParam;
use infinitedsp_core::effects::dynamics::distortion::{Distortion, DistortionType};
use infinitedsp_core::effects::time::ping_pong_delay::PingPongDelay;
use infinitedsp_core::effects::time::reverb::Reverb;

pub mod analog_bass_drum;
pub mod bloom;
pub mod chord;
pub mod fm_stab;
pub mod hihat;
pub mod midi;
pub mod midi_map;
pub mod sequencer;
pub mod svf;
pub mod tape;
pub mod timeline;

pub use analog_bass_drum::AnalogBassDrum;
pub use fm_stab::{FmPatch, FmStab, Shaper};
pub use hihat::HiHat;
pub use midi::MidiMessage;
pub use midi_map::{MidiMap, Param};
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
    /// Gain multiplier applied to the kick's output. Set per-trigger from
    /// MIDI velocity (0..1), so soft pad hits play a quieter kick.
    kick_velocity: f32,
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
    tape: tape::TapeProcessor,
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
            1.0,                          // max delay buffer, seconds
            AudioParam::seconds(0.375),   // dotted-8th-ish at techno tempo
            AudioParam::linear(0.45),     // feedback → a few repeats
            AudioParam::linear(1.0),      // mix = wet-only output
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
                s.load_patch(fm_stab::FmPatch::industrial());
                s
            },
            stab_buf: Vec::new(),
            stab_send: Vec::new(),
            stab_delay,
            stab_delay_wet: 0.5,
            kick_velocity: 1.0,
            kick_trigger_note: 60,
            midi_map: MidiMap::new(),
            sequencer: sequencer::Sequencer::new(sample_rate),
            reverb,
            bloom: bloom::BloomBank::new(sample_rate),
            tape: tape::TapeProcessor::new(sample_rate),
            dry_scratch: Vec::new(),
            sample_index: 0,
            // A wash of room by default — the cold, spacious Cybotron air. Dial
            // with `set_reverb_wet` / the ReverbWet CC.
            reverb_wet: 0.18,
            sequencer_enabled: true,
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
                self.stabs.play_chord(hit.chord.notes(), hit.velocity);
            }
            let st = self.stabs.tick();
            self.stab_buf[i] = st;
            // Feed the delay send on the left only; the cross-feedback makes
            // the echoes ping-pong across the stereo field.
            self.stab_send[2 * i] = st;
            self.stab_send[2 * i + 1] = 0.0;
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
            let kick_mix = k * vel;
            let hat_mix = h * hh_gain;
            out_frame[0] += kick_mix + hat_mix + st;
            out_frame[1] += kick_mix + hat_mix + st;
        }

        // 2b. Ping-pong delay on the stab send (wet-only) — fold the bouncing
        //     echoes into the master *before* the reverb so each repeat trails
        //     off into the room. Cybotron in a box.
        self.stab_delay.process(&mut self.stab_send, self.sample_index);
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

        // 6. Tape emulation — final stage on the master bus. Currently
        //    Phase 1 (head bump + hiss); wow/flutter, loss filter, and
        //    hysteresis chain in here as they land. See TAPE_SIMULATION.md.
        self.tape.process(output, self.sample_index);

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
