//! Audition the firmware "bell on top" topology on the desktop, without the
//! Daisy. We deliberately do NOT use `dsp::Engine` here — the firmware doesn't
//! either. Instead we hand-roll the bell-side sub-graph the way firmware will:
//!
//!   FM bell (mono)
//!     ├─ dry → master bus (both channels)
//!     └─ panned hard-left → stereo send → ping-pong delay (wet-only)
//!                                              → wet folded on top of master
//!   master bus → master limiter → output
//!
//! In firmware this whole block is summed into the master *after* tape +
//! destruction but *before* the limiter (see firmware/src/main.rs:470). Here
//! there's no backing track to destroy — we're auditioning the bell + delay +
//! limiter portion so the routing and timbre can be judged in isolation.
//!
//! The bell chimes every 10 s. The preset uses `Shaper::Off` (pure-sine FM) —
//! auditioned identical to a tanh shaper and cheaper on the Daisy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SizedSample};
use dsp::limiter::Limiter;
use dsp::{AudioParam, FmPatch, FmStab, FrameProcessor as _, PingPongDelay};

/// The bell-on-top sub-graph: FM voice + ping-pong delay + master limiter,
/// summed in the same order the firmware master bus will use.
struct BellRig {
    bell: FmStab,
    delay: PingPongDelay,
    limiter: Limiter,
    /// Stereo-interleaved send buffer for the delay (resized to the block).
    send: Vec<f32>,
    /// How much of the wet ping-pong to fold on top of the dry bell.
    wet: f32,
    sample_index: u64,
}

impl BellRig {
    fn new(sample_rate: f32) -> Self {
        let mut bell = FmStab::new(sample_rate);
        bell.load_patch(FmPatch::bell());

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
            sample_index: 0,
        }
    }

    /// Grow the delay's internal scratch up front so the real-time callback
    /// never allocates — the discipline firmware requires (it can't alloc in
    /// the audio IRQ; firmware primes tape the same way).
    fn prime(&mut self) {
        let mut scratch = vec![0.0f32; 2048 * 2];
        self.delay.process(&mut scratch, 0);
    }

    /// Strike the bell (patch is loaded once in `new`).
    fn chime(&mut self) {
        self.bell.note_on(93, 1.0); // A6 = A440 up two octaves = 1760 Hz
    }

    /// Render `out.len()/2` stereo frames (interleaved) of the full sub-graph.
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

fn main() -> Result<()> {
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

    let rig = Arc::new(Mutex::new(BellRig::new(sample_rate)));
    rig.lock().unwrap().prime();

    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, rig.clone())?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, rig.clone())?,
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, rig.clone())?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;

    // Chime every 10 s. Ctrl-C to stop.
    println!("chiming every 10 s — Ctrl-C to stop");
    loop {
        rig.lock().unwrap().chime();
        println!("chime");
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    rig: Arc<Mutex<BellRig>>,
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
