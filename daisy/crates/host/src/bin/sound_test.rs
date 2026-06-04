//! Audition the firmware's "on top of the mix" foreground voices on the desktop,
//! without the Daisy. We deliberately do NOT use `dsp::Engine` here — the
//! firmware doesn't either. Instead we hand-roll each voice's sub-graph the way
//! firmware does, summed in the same order (after tape + destruction, before the
//! master limiter — see firmware/src/main.rs). There's no backing track here, so
//! each voice is judged in isolation.
//!
//! Pick what to audition with a CLI arg:
//!
//!   cargo run -p host --bin sound_test            # bell (default)
//!   cargo run -p host --bin sound_test -- bell        # FM bell (ch0 patch)
//!   cargo run -p host --bin sound_test -- industrial  # industrial stab (ch1 patch)
//!   cargo run -p host --bin sound_test -- voice       # "pain material" speech
//!   cargo run -p host --bin sound_test -- voice --every=8
//!
//! The selected voice is triggered every `--every` seconds (default 10).
//! bell/industrial share the FM bank + ping-pong delay path (the firmware swaps
//! only the patch); voice is the formant SpeechSynth through its own reverb.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SizedSample};
use dsp::limiter::Limiter;
use dsp::{AudioParam, FmPatch, FmStab, FrameProcessor as _, PainMaterialVoice, PingPongDelay};

/// Which foreground voice to audition.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Bell,
    Industrial,
    Voice,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Bell => "bell",
            Mode::Industrial => "industrial",
            Mode::Voice => "voice",
        }
    }
}

/// One auditionable voice: trigger it, then render interleaved-stereo blocks.
trait Rig: Send {
    /// Strike / start the voice; returns a label for what was triggered (the
    /// phrase text for the voice, the voice name otherwise).
    fn trigger(&mut self) -> &'static str;
    /// Render `out.len()/2` stereo frames (interleaved) in place.
    fn render(&mut self, out: &mut [f32]);
    /// Grow internal scratch up front so the callback never allocates.
    fn prime(&mut self) {}
}

/// The FM-bank sub-graph (bell OR industrial): FM voice + ping-pong delay +
/// master limiter, summed in the same order the firmware master bus uses. The
/// only difference between bell and industrial is the loaded patch — exactly
/// how the firmware does it (it swaps the patch on the shared FmStab per strike).
struct BellRig {
    bell: FmStab,
    delay: PingPongDelay,
    limiter: Limiter,
    /// Stereo-interleaved send buffer for the delay (resized to the block).
    send: Vec<f32>,
    /// How much of the wet ping-pong to fold on top of the dry bell.
    wet: f32,
    /// MIDI note struck on `trigger`.
    note: u8,
    /// Label printed on each strike ("bell" / "industrial").
    label: &'static str,
    sample_index: u64,
}

impl BellRig {
    fn new(sample_rate: f32, patch: FmPatch, note: u8, label: &'static str) -> Self {
        let mut bell = FmStab::new(sample_rate);
        bell.load_patch(patch);

        // Firmware-realistic delay sizing. The first ctor arg is the *max*
        // buffer in seconds, and that's what gets allocated: at 48 kHz stereo
        // f32, 0.25 s ≈ 96 KB — fits the firmware's 256 KB AXI-SRAM heap
        // (the Engine's 1.0 s default would be ~384 KB and overflow it). Keep
        // the actual delay time under that ceiling. `mix = 1.0` → wet-only
        // output, so we scale the wet ourselves when summing, exactly like the
        // Engine's stab bus.
        let mut delay = PingPongDelay::new(
            0.25,                       // max delay buffer, seconds (fits AXI heap)
            AudioParam::seconds(0.22),  // ~quarter-note bounce, < the 0.25 s ceiling
            AudioParam::linear(0.55),   // feedback → a few L<->R repeats
            AudioParam::linear(1.0),    // mix = wet-only
        );
        delay.set_sample_rate(sample_rate);

        BellRig {
            bell,
            delay,
            limiter: Limiter::new(sample_rate),
            send: Vec::new(),
            wet: 0.6,
            note,
            label,
            sample_index: 0,
        }
    }
}

impl Rig for BellRig {
    fn prime(&mut self) {
        // Grow the delay's internal scratch up front so the real-time callback
        // never allocates — the discipline firmware requires (it can't alloc in
        // the audio IRQ; firmware primes tape the same way).
        let mut scratch = vec![0.0f32; 2048 * 2];
        self.delay.process(&mut scratch, 0);
    }

    fn trigger(&mut self) -> &'static str {
        self.bell.note_on(self.note, 1.0);
        self.label
    }

    fn render(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        self.send.resize(frames * 2, 0.0);

        for i in 0..frames {
            let s = self.bell.tick(); // mono bell sample
            out[2 * i] = s; // dry bell on both channels...
            out[2 * i + 1] = s;
            self.send[2 * i] = s; // ...and into the delay send, left only —
            self.send[2 * i + 1] = 0.0; // the cross-feedback bounces it L<->R.
        }

        // Ping-pong the send (wet-only), then fold the wet on top of the dry.
        self.delay.process(&mut self.send, self.sample_index);
        let wet = self.wet;
        for (o, &w) in out.iter_mut().zip(self.send.iter()) {
            *o += w * wet;
        }

        // Master limiter — in firmware the bell is summed *before* this stage.
        self.limiter.process(out);

        self.sample_index += frames as u64;
    }
}

/// The "pain material" speech sub-graph: PainMaterialVoice (SpeechSynth + its
/// own reverb) → master limiter, exactly the firmware's voice slot. The voice
/// renders its own wet/dry blend internally, so the rig just limits the sum.
struct VoiceRig {
    voice: PainMaterialVoice,
    limiter: Limiter,
    /// Largest stereo block the voice was sized for; longer cpal buffers are
    /// rendered in chunks of this size so the internal scratch never overflows.
    cap: usize,
    /// Cycles through the phrases so each one can be auditioned in turn.
    next: usize,
    sample_index: u64,
}

impl VoiceRig {
    fn new(sample_rate: f32) -> Self {
        let cap = 4096; // interleaved stereo samples (2048 frames) per chunk
        VoiceRig {
            voice: PainMaterialVoice::new(sample_rate, cap),
            limiter: Limiter::new(sample_rate),
            cap,
            next: 0,
            sample_index: 0,
        }
    }
}

impl Rig for VoiceRig {
    fn trigger(&mut self) -> &'static str {
        // Cycle the phrases so auditioning hears each in rotation (the install
        // picks at random instead).
        let idx = self.next % dsp::pain_voice::PHRASE_COUNT;
        self.next += 1;
        self.voice.trigger_phrase(idx, 1.0);
        dsp::pain_voice::PHRASE_LABELS[idx]
    }

    fn render(&mut self, out: &mut [f32]) {
        let mut off = 0;
        while off < out.len() {
            let end = (off + self.cap).min(out.len());
            let chunk = &mut out[off..end];
            self.voice.process(chunk, self.sample_index);
            self.sample_index += (chunk.len() / 2) as u64;
            off = end;
        }
        self.limiter.process(out);
    }
}

/// Parse `[bell|industrial|voice] [--every=SECS]`. Defaults: bell, 10 s.
fn parse_args() -> (Mode, u64) {
    let mut mode = Mode::Bell;
    let mut every = 10u64;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "bell" => mode = Mode::Bell,
            "industrial" => mode = Mode::Industrial,
            "voice" | "pain" | "pain-material" => mode = Mode::Voice,
            "-h" | "--help" => {
                println!(
                    "usage: sound_test [bell|industrial|voice] [--every=SECS]\n\
                     \n\
                     bell        FM bell (firmware ch0 patch)\n\
                     industrial  industrial stab (firmware ch1 patch)\n\
                     voice       formant speech through reverb, cycling all phrases\n\
                     --every=N   seconds between triggers (default 10)"
                );
                std::process::exit(0);
            }
            s if s.starts_with("--every=") => {
                match s["--every=".len()..].parse::<u64>() {
                    Ok(n) if n > 0 => every = n,
                    _ => {
                        eprintln!("bad --every value {s:?}; want a positive integer");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("unknown arg {other:?}; use bell | industrial | voice [--every=SECS]");
                std::process::exit(2);
            }
        }
    }
    (mode, every)
}

fn main() -> Result<()> {
    let (mode, every) = parse_args();

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0 as f32;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();

    println!(
        "output: {}  sr={} Hz  ch={}  fmt={:?}",
        device.name().unwrap_or_else(|_| "<unnamed>".into()),
        sample_rate,
        channels,
        format,
    );

    // Build the chosen voice. bell/industrial are the same FM rig with a
    // different patch (note 81 = A5, the install default); voice is the speech
    // rig, which cycles through all phrases.
    let rig: Arc<Mutex<dyn Rig + Send>> = match mode {
        Mode::Bell => Arc::new(Mutex::new(BellRig::new(sample_rate, FmPatch::bell(), 81, "bell"))),
        Mode::Industrial => Arc::new(Mutex::new(BellRig::new(
            sample_rate,
            FmPatch::industrial(),
            81,
            "industrial",
        ))),
        Mode::Voice => Arc::new(Mutex::new(VoiceRig::new(sample_rate))),
    };
    rig.lock().unwrap().prime();

    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, rig.clone())?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, rig.clone())?,
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, rig.clone())?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;

    // Trigger the selected voice every `every` seconds. Ctrl-C to stop.
    println!("auditioning '{}' every {every} s — Ctrl-C to stop", mode.label());
    loop {
        let what = rig.lock().unwrap().trigger();
        println!("{what}");
        std::thread::sleep(Duration::from_secs(every));
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    rig: Arc<Mutex<dyn Rig + Send>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], _: &cpal::OutputCallbackInfo| {
            let frames = output.len() / channels;
            scratch.resize(frames * 2, 0.0);
            rig.lock().unwrap().render(&mut scratch);

            for (cpal_frame, dsp_frame) in
                output.chunks_exact_mut(channels).zip(scratch.chunks_exact(2))
            {
                let l = dsp_frame[0];
                let r = dsp_frame[1];
                if channels == 1 {
                    cpal_frame[0] = T::from_sample(0.5 * (l + r));
                } else {
                    cpal_frame[0] = T::from_sample(l);
                    cpal_frame[1] = T::from_sample(r);
                    for ch in &mut cpal_frame[2..] {
                        *ch = T::from_sample(0.0);
                    }
                }
            }
        },
        |err| eprintln!("stream error: {err}"),
        None,
    )?;
    Ok(stream)
}
