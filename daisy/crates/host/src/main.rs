//! macOS dev host. Opens the system default output device via cpal and feeds
//! it from `dsp::Engine` — the same Engine that runs on the Daisy firmware.
//!
//! Usage:
//!   cargo run -p host --release -- <path-to-audio-file>
//!
//! Without a path, the output is silent (engine still runs).

use std::env;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dsp::Param;
use midir::{Ignore, MidiInput};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn main() -> Result<()> {
    // First non-flag argument is the audio path; `--no-seq` runs the engine
    // with the step sequencer disabled (track + processing, no drum pattern).
    // `--test-mod` restores the old exhibit audition scaffolding that pins
    // bloom and pulses freeze. It is off by default for the groovebox harness.
    let args: Vec<String> = env::args().skip(1).collect();
    let no_seq = args.iter().any(|a| a == "--no-seq");
    let test_mod = args.iter().any(|a| a == "--test-mod");
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
    {
        let mut eng = engine.lock().unwrap();
        eng.sequencer_mut()
            .set_tempo(dsp::timeline::fixed_bpm(120.0), 8.0);
    }

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

    // MIDI CC bindings — shared with the Daisy firmware via
    // dsp::install_kiosk_bindings so a CC means the same thing in both. Trigger:
    // MIDI note 36 (C1, GM kick) fires the kick on note-on. Incoming MIDI is
    // printed below so you can discover what a controller's knobs emit.
    dsp::install_kiosk_bindings(engine.lock().unwrap().midi_map_mut());

    // Connect to a MIDI input. midir owns the callback thread; the connection
    // must stay alive (we bind it to a named local so it lives till main exits).
    let _midi_conn = connect_midi(Arc::clone(&engine))?;
    spawn_groove_stdin(Arc::clone(&engine));

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
    print_groove_help();

    // No MIDI knob handy, so drive the resonant-bloom "proximity" with an
    // internal LFO standing in for the ToF distance sensor: one smooth
    // far → near → far sweep every 8 bars at 112 BPM (= 17.143 s). A raised
    // cosine starts at 0 (far/silent), peaks at 1 (full D-Lydian bloom) at the
    // half-period, and returns to 0. This is host-only test scaffolding — the
    // real exhibit drives `set_bloom_amount` from the kiosk distance sensor.
    if test_mod {
        use std::f32::consts::PI;
        let bloom_engine = Arc::clone(&engine);
        let period_s = 8.0 * 4.0 * 60.0 / 112.0; // 8 bars · 4 beats · (60/BPM)
        println!("bloom amount PINNED at 0.9 for tuning (LFO period would be {period_s:.3}s)");
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let dt = std::time::Duration::from_millis(10); // 100 Hz control rate
            loop {
                std::thread::sleep(dt);
                let t = start.elapsed().as_secs_f32();
                let _sweep = 0.5 - 0.5 * ((t / period_s) * 2.0 * PI).cos(); // far→near→far LFO
                                                                            // AUDITION: pinned high so the bloom is judged at the peak of
                                                                            // the gesture, not the fleeting LFO crest. Swap to `_sweep` to
                                                                            // restore the far→near→far sweep.
                let amount = 0.9;
                bloom_engine.lock().unwrap().set_bloom_amount(amount);
            }
        });
    }

    // Master-freeze test: hold a grain for ~0.5 s once every ~10 s, then
    // release. Host-only scaffolding — the real exhibit drives `set_freeze`
    // from the visualizer's JS freeze over the (unconnected) CDC path.
    if test_mod {
        let freeze_engine = Arc::clone(&engine);
        println!("freeze test: holding ~0.5s every ~10s");
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(9500));
            freeze_engine.lock().unwrap().set_freeze(1.0);
            println!("  freeze ON");
            std::thread::sleep(std::time::Duration::from_millis(500));
            freeze_engine.lock().unwrap().set_freeze(0.0);
            println!("  freeze OFF");
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

fn spawn_groove_stdin(engine: Arc<Mutex<dsp::Engine>>) {
    std::thread::spawn(move || {
        let mut state = GrooveboxHostState::default();
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                eprintln!("groove stdin: read error");
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if trimmed.eq_ignore_ascii_case("HELP") || trimmed == "?" {
                print_groove_help();
                continue;
            }

            if trimmed.eq_ignore_ascii_case("STATE") {
                let eng = engine.lock().unwrap();
                println!("{}", state.snapshot(&eng));
                continue;
            }

            match dsp::groove::parse_line(trimmed) {
                Ok(evt) => {
                    let mut eng = engine.lock().unwrap();
                    eng.handle_groove_event(evt);
                    state.observe_event(evt);
                    println!("  groove {:?}", evt);
                    println!("{}", state.snapshot(&eng));
                }
                Err(e) => {
                    eprintln!("  groove parse error {:?}: {}", e, trimmed);
                    print_groove_help();
                }
            }
        }
    });
}

fn print_groove_help() {
    println!("groove stdin commands:");
    println!("  HELP | STATE");
    println!("  PLAY 1 | STOP | RESET | TRACK kick");
    println!("  PAD 36 127 | TOGGLE kick 0 | STEP bass 4 96");
    println!("  BASS 4 hold | BASS 4 rest | PBASS 1 4 tie");
    println!("  PATTERN 1 | CAPTURE 1 | PCOPY 1 2 | PCLEAR 2");
    println!("  PFILL 1 kick 127 | PRAND 1 kick 42 64 127");
    println!("  MACRO damage 64 | MACRO space 96 | MACRO tone 80");
    println!("  MACRO filter_cutoff 80 | MACRO filter_resonance 48 | MACRO filter_motion 96");
    println!("  BAND 1 | FILTER cutoff 80 | FILTER 3 q 48 | FILTER 3 motion 96");
}

#[derive(Debug, Default)]
struct GrooveboxHostState {
    macros: [Option<f32>; 10],
}

impl GrooveboxHostState {
    fn observe_event(&mut self, evt: dsp::GrooveEvent) {
        if let dsp::GrooveEvent::SetMacro { macro_id, value } = evt {
            if let Some(slot) = self.macros.get_mut(macro_id as usize) {
                *slot = Some(value.clamp(0.0, 1.0));
            }
        }
    }

    fn snapshot(&self, eng: &dsp::Engine) -> String {
        let seq = eng.sequencer();
        let step = seq.step() as usize;
        let loop_steps = seq.steps_per_loop().max(1);
        let display_step = step % loop_steps;
        let selected = eng.selected_track();
        let selected_step = selected_step_value(eng, selected, display_step);
        let dyn_settings = eng.spectre_dynamic_filter_settings();
        let band_idx = eng
            .selected_filter_band()
            .min(dyn_settings.bands.len().saturating_sub(1));
        let band = dyn_settings.bands[band_idx];
        let envelopes = eng.spectre_dynamic_envelopes();
        let band_envelope = envelopes[band_idx];
        let master = eng.spectre_filter_settings();

        format!(
            concat!(
                "  state transport={} pattern={} pending_pattern={} track={} step={}/{} spb={} selected_step={}\n",
                "  macros damage={} space={} tone={} levels[k={} h={} st={} b={}]\n",
                "  filter band={} env={:.3} enabled={} mode={:?} ch={:?} freq={:.1}Hz q={:.2} dyn={:+.1}dB sweep={:.2}oct\n",
                "  master_filter model={:?} cutoff={:.1}Hz res={:.2} drive={:.2} morph={:.2} mix={:.2}"
            ),
            if eng.sequencer_enabled() {
                "play"
            } else {
                "stop"
            },
            eng.pattern_bank().selected() + 1,
            eng.pending_pattern_slot()
                .map(|slot| (slot + 1).to_string())
                .unwrap_or_else(|| "--".to_string()),
            track_name(selected),
            display_step,
            loop_steps,
            seq.steps_per_beat(),
            selected_step,
            self.macro_value(dsp::Macro::Damage),
            self.macro_value(dsp::Macro::Space),
            self.macro_value(dsp::Macro::Tone),
            self.macro_value(dsp::Macro::KickLevel),
            self.macro_value(dsp::Macro::HatLevel),
            self.macro_value(dsp::Macro::StabLevel),
            self.macro_value(dsp::Macro::BassLevel),
            band_idx + 1,
            band_envelope,
            band.enabled,
            band.mode,
            band.channel_mode,
            band.frequency_hz,
            band.q,
            band.dynamic_db,
            band.sweep_octaves,
            master.model,
            master.cutoff_hz,
            master.resonance,
            master.drive,
            master.morph,
            master.mix,
        )
    }

    fn macro_value(&self, m: dsp::Macro) -> String {
        self.macros
            .get(m.id() as usize)
            .and_then(|v| *v)
            .map(format_normalized)
            .unwrap_or_else(|| "--".to_string())
    }
}

fn selected_step_value(eng: &dsp::Engine, track: dsp::Track, step: usize) -> String {
    match track {
        dsp::Track::Bass => eng
            .sequencer()
            .bass_step(step)
            .map(|cell| match cell {
                dsp::sequencer::BassCell::Rest => "rest".to_string(),
                dsp::sequencer::BassCell::Hold => "hold".to_string(),
                dsp::sequencer::BassCell::Strike(v) => format!("strike:{v:.2}"),
            })
            .unwrap_or_else(|| "n/a".to_string()),
        _ => sequencer_voice(track)
            .and_then(|voice| eng.sequencer().step_velocity(voice, step))
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "n/a".to_string()),
    }
}

fn sequencer_voice(track: dsp::Track) -> Option<dsp::sequencer::Voice> {
    match track {
        dsp::Track::Kick => Some(dsp::sequencer::Voice::Kick),
        dsp::Track::ClosedHat => Some(dsp::sequencer::Voice::Chat),
        dsp::Track::OpenHat => Some(dsp::sequencer::Voice::Ohat),
        dsp::Track::Stab => Some(dsp::sequencer::Voice::Stab),
        dsp::Track::Bass => None,
    }
}

fn track_name(track: dsp::Track) -> &'static str {
    match track {
        dsp::Track::Kick => "kick",
        dsp::Track::ClosedHat => "closed_hat",
        dsp::Track::OpenHat => "open_hat",
        dsp::Track::Stab => "stab",
        dsp::Track::Bass => "bass",
    }
}

fn format_normalized(value: f32) -> String {
    format!("{value:.2}")
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
