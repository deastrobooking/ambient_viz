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
use dsp::Param;
use midir::{Ignore, MidiInput};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn main() -> Result<()> {
    // First non-flag argument is the audio path; `--no-seq` runs the engine
    // with the step sequencer disabled (track + processing, no drum pattern).
    let args: Vec<String> = env::args().skip(1).collect();
    let no_seq = args.iter().any(|a| a == "--no-seq");
    let audio_path = args.into_iter().find(|a| !a.starts_with("--"));

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

    // `loop_seconds` is the loaded sample's duration; the BPM thread uses
    // it to map wall-clock time back to a position in the loop. `mp3_path`
    // is kept so we can probe for a sidecar `<basename>.timeline.json`.
    let mut loop_seconds: Option<f32> = None;
    let mut mp3_path: Option<std::path::PathBuf> = None;

    if let Some(path_str) = audio_path.as_deref() {
        let path = Path::new(path_str);
        let (pcm, src_sr) =
            decode_to_stereo_f32(path).with_context(|| format!("decoding {}", path.display()))?;
        let frames = pcm.len() / 2;
        let dur = frames as f32 / src_sr;
        let leaked: &'static [f32] = Box::leak(pcm.into_boxed_slice());
        println!(
            "loaded sample: {} frames ({:.1}s at {} Hz, {:.1} MB)",
            frames,
            dur,
            src_sr as u32,
            (leaked.len() * 4) as f32 / 1024.0 / 1024.0,
        );
        let mut eng = engine.lock().unwrap();
        eng.load_sample(leaked, src_sr);
        eng.play(true);
        loop_seconds = Some(dur);
        mp3_path = Some(path.to_path_buf());
    } else {
        eprintln!(
            "no audio path provided — output will be silent.\n  usage: cargo run -p host -- <file> [--no-seq]"
        );
    }

    // Make the kick obviously audible — DaisySP's defaults (50 Hz, accent 0.1)
    // are below most laptop-speaker rolloff and get masked by full-range
    // music samples.
    {
        let mut eng = engine.lock().unwrap();
        let kick = eng.kick_mut();
        kick.set_freq(50.0); // up from 50 Hz default → punches through laptop speakers
        kick.set_accent(0.57); // up from 0.1 → louder, beefier
        kick.set_decay(0.4); // up from 0.3 → longer ring
        kick.set_tone(0.4); // up from 0.1 → more click on top
        kick.set_self_fm_amount(0.35); // stronger pitch dive (the "vrrm")
        kick.set_attack_fm_amount(0.); // cleaner pitch sweep
        eng.apply_param(Param::KickDistDrive, 6.0);
    }

    if no_seq {
        engine.lock().unwrap().set_sequencer_enabled(false);
        println!("--no-seq: step sequencer disabled (no kick/hat/stab triggers)");
    }

    // Default MIDI CC bindings. CCs 71-76 are the GM "Sound Controllers"
    // and a common starting point for hardware drum-machine knobs. Your
    // controller may send different CCs — incoming MIDI is printed below
    // so you can discover what each knob emits and edit this block.
    //
    // Trigger: MIDI note 36 (C1, GM kick) fires the kick on note-on.
    {
        use dsp::Param;
        let mut eng = engine.lock().unwrap();
        let m = eng.midi_map_mut();
        m.bind_cc(12, Param::KickAccent, 0.0, 1.0);
        m.bind_cc(13, Param::KickDecay, 0.0, 1.0);
        m.bind_cc(15, Param::KickAttackFm, 0.0, 1.0);
        m.bind_cc(16, Param::KickFreq, 30.0, 150.0);
        m.bind_cc(18, Param::KickTone, 0.0, 1.0);
        m.bind_cc(19, Param::KickSelfFm, 0.0, 1.0);
        m.bind_cc(21, Param::ReverbWet, 0.0, 1.0); // CC 91 = "Effects 1" (reverb send)
        m.bind_cc(22, Param::KickDistDrive, 1.0, 6.0); // CC 93 = "Effects 3"
        m.bind_cc(23, Param::TapeFailure, 0.0, 1.0); // 0 = pristine TC-250, 1 = eaten tape
    }

    // Connect to a MIDI input. midir owns the callback thread; the connection
    // must stay alive (we bind it to a named local so it lives till main exits).
    let _midi_conn = connect_midi(Arc::clone(&engine))?;

    let config: cpal::StreamConfig = supported.config();
    let stream_engine = Arc::clone(&engine);
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, stream_engine, channels)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, stream_engine, channels)?,
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, stream_engine, channels)?,
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };

    stream.play()?;
    println!("playing — Ctrl+C to stop");

    // No MIDI knob handy, so drive the resonant-bloom "proximity" with an
    // internal LFO standing in for the ToF distance sensor: one smooth
    // far → near → far sweep every 8 bars at 112 BPM (= 17.143 s). A raised
    // cosine starts at 0 (far/silent), peaks at 1 (full D-Lydian bloom) at the
    // half-period, and returns to 0. This is host-only test scaffolding — the
    // real exhibit drives `set_bloom_amount` from the kiosk distance sensor.
    {
        use std::f32::consts::PI;
        let bloom_engine = Arc::clone(&engine);
        let period_s = 8.0 * 4.0 * 60.0 / 112.0; // 8 bars · 4 beats · (60/BPM)
        println!("bloom LFO: far→near→far every {period_s:.3}s (8 bars @ 112 BPM)");
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let dt = std::time::Duration::from_millis(10); // 100 Hz control rate
            loop {
                std::thread::sleep(dt);
                let t = start.elapsed().as_secs_f32();
                let phase = (t / period_s) * 2.0 * PI;
                let amount = 0.5 - 0.5 * phase.cos(); // 0 → 1 → 0 over the period
                bloom_engine.lock().unwrap().set_bloom_amount(amount);
            }
        });
    }

    if false {
        // TEST: hold pristine for 10 s, then ramp tape failure 0 → 1 over the
        // next 10 s, and hold at full destruction. Demonstrates the lerp + the
        // 50 ms smoothing inside `set_failure` (per-step jumps are absorbed).
        {
            let failure_engine = Arc::clone(&engine);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                println!("== tape failure ramp begins (0 → 1 over 10 s) ==");
                let steps = 100u32;
                let dt = std::time::Duration::from_millis(100); // 10 Hz update
                for i in 1..=steps {
                    std::thread::sleep(dt);
                    let amount = i as f32 / steps as f32;
                    failure_engine
                        .lock()
                        .unwrap()
                        .tape_mut()
                        .set_failure(amount);
                }
                println!("== tape failure pinned at 1.0 — listen for the chaos ==");
            });
        }
    }

    // If there's a sidecar `<basename>.timeline.json` next to the MP3,
    // load the BPM lane and print the interpolated tempo every second.
    if let (Some(dur), Some(path)) = (loop_seconds, mp3_path.as_ref()) {
        let timeline_path = path.with_extension("timeline.json");
        match std::fs::read(&timeline_path) {
            Ok(bytes) => match dsp::timeline::parse_bpm(&bytes) {
                Some(keypoints) if !keypoints.is_empty() => {
                    println!(
                        "loaded {} BPM keypoints from {}",
                        keypoints.len(),
                        timeline_path.display(),
                    );
                    // Lock the kick sequencer to the song's tempo curve.
                    // Default pattern is all-on (kick on every beat).
                    {
                        let mut eng = engine.lock().unwrap();
                        eng.sequencer_mut().set_tempo(keypoints.clone(), dur);
                    }

                    // Sibling `.pat` drum-grid file. If present, override the
                    // built-in default pattern with whatever's in the file.
                    // Parse errors are logged but non-fatal (defaults stay).
                    let pat_path = path.with_extension("pat");
                    match std::fs::read_to_string(&pat_path) {
                        Ok(text) => {
                            let mut eng = engine.lock().unwrap();
                            match eng.sequencer_mut().load_grid(&text) {
                                Ok(grid) => println!(
                                    "loaded pattern '{}' ({} steps) from {}",
                                    grid.name.as_str(),
                                    grid.steps,
                                    pat_path.display(),
                                ),
                                Err(e) => eprintln!(
                                    "pattern parse error in {}: {:?} — using built-in defaults",
                                    pat_path.display(),
                                    e,
                                ),
                            }
                        }
                        Err(_) => println!(
                            "(no pattern at {} — using built-in defaults)",
                            pat_path.display(),
                        ),
                    }
                    let count_engine = Arc::clone(&engine);
                    std::thread::spawn(move || {
                        // Wall clock from this moment as the playback ref.
                        // Stream is already running, so drift vs. true audio
                        // position is sub-frame.
                        let start = std::time::Instant::now();
                        let (mut last_k, mut last_c, mut last_o) = (0u64, 0u64, 0u64);
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            let elapsed = start.elapsed().as_secs_f32();
                            let t = if dur > 0.0 { elapsed % dur } else { elapsed };
                            let bpm = dsp::timeline::bpm_at(&keypoints, t);
                            let (k, c, o) = {
                                let eng = count_engine.lock().unwrap();
                                let s = eng.sequencer();
                                (s.kick_count(), s.closed_hat_count(), s.open_hat_count())
                            };
                            let (dk, dc, do_) = (k - last_k, c - last_c, o - last_o);
                            last_k = k;
                            last_c = c;
                            last_o = o;
                            println!("  tempo: {bpm:.2} BPM  (t={t:.1}s)  +K{dk} +CH{dc} +OH{do_}",);
                        }
                    });
                }
                _ => println!("timeline {} has no bpm lane", timeline_path.display()),
            },
            Err(_) => println!(
                "(no timeline at {} — tempo display off)",
                timeline_path.display()
            ),
        }
    }

    std::thread::park();
    Ok(())
}

/// Connect to a MIDI input port and forward decoded messages to the engine.
/// Returns the connection handle, which must be kept alive for input to flow.
/// Port is selected by the `MIDI_PORT` env var (index into `midi_in.ports()`),
/// defaulting to 0. Returns `Ok(None)` if no MIDI ports exist.
fn connect_midi(engine: Arc<Mutex<dsp::Engine>>) -> Result<Option<midir::MidiInputConnection<()>>> {
    let mut midi_in = MidiInput::new("ambient-viz-daisy")?;
    midi_in.ignore(Ignore::None);

    let ports = midi_in.ports();
    if ports.is_empty() {
        eprintln!("no MIDI input ports found — kick won't trigger until you connect a device");
        return Ok(None);
    }

    println!("MIDI ports:");
    for (i, p) in ports.iter().enumerate() {
        println!("  [{i}] {}", midi_in.port_name(p).unwrap_or_default());
    }
    let idx = std::env::var("MIDI_PORT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(ports.len() - 1);
    let port = &ports[idx];
    println!(
        "→ connecting to [{idx}] {}",
        midi_in.port_name(port).unwrap_or_default(),
    );

    let conn = midi_in
        .connect(
            port,
            "midi-in",
            move |_timestamp, bytes, _| {
                if let Some(msg) = dsp::midi::decode(bytes) {
                    // For ControlChange we also resolve through the map so the
                    // log shows exactly what param/value the engine will apply.
                    match msg {
                        dsp::MidiMessage::ControlChange { channel, cc, value } => {
                            let mapped =
                                engine.lock().unwrap().midi_map().map_cc(cc, value);
                            match mapped {
                                Some((param, mapped_value)) => println!(
                                    "  midi ch{channel} CC#{cc} = {value} → {param:?} = {mapped_value:.3}"
                                ),
                                None => println!("  midi ch{channel} CC#{cc} = {value}"),
                            }
                        }
                        dsp::MidiMessage::NoteOn { note, velocity, .. } => {
                            println!("  midi note-on {note} vel {velocity}");
                        }
                        dsp::MidiMessage::NoteOff { note, .. } => {
                            println!("  midi note-off {note}");
                        }
                        dsp::MidiMessage::PitchBend { value, .. } => {
                            println!("  midi pitch-bend {value}");
                        }
                    }
                    engine.lock().unwrap().handle_midi(msg);
                }
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("midir connect failed: {e}"))?;
    Ok(Some(conn))
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

            for (cpal_frame, dsp_frame) in output
                .chunks_exact_mut(channels)
                .zip(scratch.chunks_exact(2))
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
