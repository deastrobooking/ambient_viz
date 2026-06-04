//! Test sounds in the DSP engine without creating the full stack.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SizedSample};
use dsp::{Engine, FmPatch};

fn main() -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

    let supported = device.default_output_config()?;
    let engine_sample_rate = supported.sample_rate().0 as f32;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();

    println!(
        "output: {}  sr={} Hz  ch={}  fmt={:?}",
        device.name().unwrap_or_else(|_| "<unnamed>".into()),
        engine_sample_rate,
        channels,
        format,
    );

    let engine = Arc::new(Mutex::new(Engine::new(engine_sample_rate)));

    // Load the bell patch and strike A6 (A440 up two octaves = 1760 Hz).
    {
        let mut eng = engine.lock().unwrap();
        let bank = eng.stabs_mut();
        bank.load_patch(FmPatch::bell());
        bank.note_on(93, 1.0); // use 105 for A7 = 3520 Hz if you want it higher
    }
    println!("ding");

    // Build and START an output stream that pulls from the engine each block.
    // Without this, nothing is ever rendered and you hear silence.
    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, engine)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, engine)?,
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, engine)?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;

    // Keep the process (and the stream) alive while the ~5 s bell rings out.
    std::thread::sleep(std::time::Duration::from_secs(6));
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    engine: Arc<Mutex<Engine>>,
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
            engine.lock().unwrap().process(&[], &mut scratch);

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
