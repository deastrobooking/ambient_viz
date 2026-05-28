#![no_std]

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
use infinitedsp_core::effects::time::reverb::Reverb;

pub mod analog_bass_drum;
pub mod svf;

pub use analog_bass_drum::AnalogBassDrum;
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
    reverb: Reverb,
    /// Holds the dry sampler output across the reverb call so we can mix wet+dry.
    dry_scratch: Vec<f32>,
    /// Global sample index, fed to FrameProcessor::process for time-aware effects.
    sample_index: u64,
    /// 0.0 = fully dry, 1.0 = fully wet.
    reverb_wet: f32,
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

        Self {
            sample_rate,
            sampler: Sampler::new(),
            kick: AnalogBassDrum::new(sample_rate),
            kick_dist,
            kick_buf: Vec::new(),
            reverb,
            dry_scratch: Vec::new(),
            sample_index: 0,
            reverb_wet: 0.,
        }
    }

    /// Load a stereo-interleaved f32 sample. `src_sample_rate` is the rate at
    /// which the buffer was recorded; playback resamples on the fly. `buf` must
    /// outlive the engine (typically `Box::leak` on host, `static` on embedded).
    pub fn load_sample(&mut self, buf: &'static [f32], src_sample_rate: f32) {
        debug_assert_eq!(buf.len() % 2, 0, "sample buffer must be interleaved stereo");
        self.sampler.load(buf, src_sample_rate / self.sample_rate);
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
    pub fn trigger_kick(&mut self) {
        self.kick.trig();
    }

    /// Mutable access to the kick drum for tweaking freq/decay/tone/etc.
    pub fn kick_mut(&mut self) -> &mut AnalogBassDrum {
        &mut self.kick
    }

    /// Mutable access to the kick-bus distortion (drive, mix, type).
    pub fn kick_dist_mut(&mut self) -> &mut Distortion {
        &mut self.kick_dist
    }

    /// Render one block. `_input` is reserved for future passthrough/sidechain;
    /// `output` (interleaved stereo) is fully overwritten.
    pub fn process(&mut self, _input: &[f32], output: &mut [f32]) {
        // 1. Sampler fills output (cleared first).
        for s in output.iter_mut() {
            *s = 0.0;
        }
        self.sampler.mix_into(output);

        // 2. Render kick into a mono scratch buffer, then soft-clip it before
        //    mixing into the stereo output (mono → both channels).
        let n_frames = output.len() / 2;
        self.kick_buf.resize(n_frames, 0.0);
        for k in self.kick_buf.iter_mut() {
            *k = self.kick.process(false);
        }
        self.kick_dist
            .process(&mut self.kick_buf, self.sample_index);
        for (out_frame, &k) in output.chunks_exact_mut(2).zip(self.kick_buf.iter()) {
            out_frame[0] += k;
            out_frame[1] += k;
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

        self.sample_index += (output.len() / 2) as u64;
    }
}

struct Sampler {
    buf: Option<&'static [f32]>,
    frames: usize,
    /// Fractional read position in frames.
    position: f32,
    /// Frames advanced per output frame. = src_rate / engine_rate.
    step: f32,
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

    fn load(&mut self, buf: &'static [f32], step: f32) {
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
                    self.position -= self.frames as f32;
                } else {
                    self.playing = false;
                    return;
                }
            }

            let pos_int = self.position as usize;
            let frac = self.position - pos_int as f32;
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
