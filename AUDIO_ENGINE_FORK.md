# Separate Audio Product Fork

This repository is being rebuilt into a separate standalone Rust audio
instrument: Daisy groovebox, synth engine, sampler, effects, project/pattern
runtime, hardware controls, and line output.

The visualizer can remain a companion display or telemetry layer, but it should
not define the core architecture.

## Runtime Shape

```text
Pads / encoders / sensors / MIDI / controller MCU
                    |
                    v
           GrooveEvent / MIDI / compact control
                    |
                    v
            daisy/crates/dsp::Engine
                    |
        +-----------+-----------+
        |                       |
 Daisy codec line out      optional telemetry
 PA / mixer / phones       Pi / browser / LEDs
```

The first success case is playable audio from Daisy line out with deterministic
timing and hands-on control.

## Keep

- `daisy/crates/dsp`: shared `no_std` DSP core.
- `daisy/crates/host`: macOS audition path for fast iteration.
- `daisy/crates/firmware`: embedded Daisy runtime.
- Existing voices/effects: kick, hats, FM stabs, rumble bass, sampler,
  sequencer, tape, freeze, bloom, reverb, ping-pong delay, limiter.
- Existing pattern-bank and shared-control work as the seed of project runtime.
- Existing USB/CDC/MIDI ideas as control and telemetry transports.

## De-Emphasize

- Browser `AnalyserNode` as the primary audio intelligence.
- Daisy-to-browser audio capture as a blocker for synth/groovebox work.
- Legacy visual effect tuning as the main roadmap.
- Desktop/plugin runtime assumptions in firmware.

## Shared Control

All surfaces should translate into `GrooveEvent` before touching the engine:

```text
keyboard/MIDI/CDC/GPIO/I2C/UI -> GrooveEvent -> Engine::handle_groove_event
```

Text protocol examples:

```text
PLAY 1
STOP
RESET
TRACK kick
PAD 36 127
TOGGLE kick 0
STEP bass 4 96
BASS 4 hold
PBASS 1 4 tie
PATTERN 1
CAPTURE 1
PCOPY 1 2
PCLEAR 2
PFILL 1 kick 127
PRAND 1 kick 42 64 127
MACRO damage 64
MACRO filter_cutoff 80
MACRO filter_resonance 48
MACRO filter_motion 96
BAND 3
FILTER cutoff 80
FILTER 4 q 48
```

Keep values 7-bit (`0..127`) until there is a proven need for a binary packing.

## Host First

Develop new behavior in `daisy/crates/host` first:

- faster iteration,
- local audio output,
- CoreMIDI input,
- command logging,
- tests.

Move features to firmware only after they have bounded state and no realtime
allocation/blocking story.

## Pi 4 Companion Testing

Use `PI4_AUDIO_TEST_DEPLOYMENT.md` for current Pi 4 deployment and setup. The Pi
is a companion for mock SSE, sensors, Daisy CDC song-position/control, and
visual sync. It is not the audio acceptance target; judge instrument audio from
Daisy codec/line out.

## Firmware Rules

Firmware should receive compact events and pre-shaped runtime specs. It should
not parse rich project files, decode codecs, resize buffers, log in steady
state, or block inside the audio path.

## Roadmap

The canonical milestone plan lives in `AGENT_MEMORY.md`.

Short form (recommended order):

1. M1 Playable Host Groovebox — TUI/shortcut layer so the host is actually performable.
2. M6 Firmware Groovebox Bridge — **critical path**: swap exhibit pipeline for `dsp::Engine` + CDC GrooveEvent routing.
3. M2 Shared Comms Contract — compact encoder after text protocol is validated on firmware.
4. M3 Pattern Bank And Project Runtime — project snapshot / save-to-SD.
5. M4 Spectre Performance Filter Suite — transient shaping + master color models.
6. M5 Nexus Voice Expansion — polyphonic synth voices after the drum engine plays from hardware.
7. M7 Companion Editor And Visual Sync — deferred.
