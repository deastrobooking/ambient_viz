//! macOS dev host. Opens the system default output device via cpal and feeds
//! it from `dsp::Engine` — the same Engine that runs on the Daisy firmware.
//!
//! Usage:
//!   cargo run -p host --release -- <path-to-audio-file>
//!
//! Without a path, the output is silent (engine still runs).

use std::env;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn main() -> Result<()> {
    let audio_path = env::args().nth(1);

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

    let engine = Arc::new(Mutex::new(dsp::Engine::new(engine_sample_rate)));

    if let Some(path) = audio_path.as_deref() {
        let path = Path::new(path);
        let (pcm, src_sr) = decode_to_stereo_f32(path)
            .with_context(|| format!("decoding {}", path.display()))?;
        let frames = pcm.len() / 2;
        let leaked: &'static [f32] = Box::leak(pcm.into_boxed_slice());
        println!(
            "loaded sample: {} frames ({:.1}s at {} Hz, {:.1} MB)",
            frames,
            frames as f32 / src_sr,
            src_sr as u32,
            (leaked.len() * 4) as f32 / 1024.0 / 1024.0,
        );
        let mut eng = engine.lock().unwrap();
        eng.load_sample(leaked, src_sr);
        eng.play(true);
    } else {
        eprintln!(
            "no audio path provided — output will be silent.\n  usage: cargo run -p host -- <file>"
        );
    }

    let config: cpal::StreamConfig = supported.config();
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, engine, channels)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, engine, channels)?,
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, engine, channels)?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };

    stream.play()?;
    println!("playing — Ctrl+C to stop");
    std::thread::park();
    Ok(())
}

/// Decode an audio file to interleaved-stereo f32. Returns (samples, source_sample_rate).
/// Mono input is duplicated to stereo; multi-channel keeps the first two channels.
fn decode_to_stereo_f32(path: &Path) -> Result<(Vec<f32>, f32)> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("symphonia probe failed")?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no decodable audio track")?
        .clone();

    let track_id = track.id;
    let src_sr = track
        .codec_params
        .sample_rate
        .context("track has no sample rate")? as f32;
    let src_channels = track
        .codec_params
        .channels
        .context("track has no channel layout")?
        .count();

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("decoder make failed")?;

    let mut pcm = Vec::<f32>::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };

        let spec = *decoded.spec();
        let cap = decoded.capacity() as u64;
        let mut sample_buf = SampleBuffer::<f32>::new(cap, spec);
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();

        match src_channels {
            1 => {
                for &s in samples {
                    pcm.push(s);
                    pcm.push(s);
                }
            }
            2 => pcm.extend_from_slice(samples),
            n => {
                for frame in samples.chunks(n) {
                    pcm.push(frame[0]);
                    pcm.push(frame.get(1).copied().unwrap_or(frame[0]));
                }
            }
        }
    }

    Ok((pcm, src_sr))
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    engine: Arc<Mutex<dsp::Engine>>,
    channels: usize,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + 'static,
{
    let mut scratch = Vec::<f32>::new();

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
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    Ok(stream)
}
