# Audio-First Fork

This fork is moving away from the browser/video installation and toward a
standalone Rust audio instrument: Daisy groovebox, synth engine, sampler,
effects, storage, hardware controls, and line output.

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
- Existing USB/CDC/MIDI ideas as control and telemetry transports.

## De-Emphasize

- Browser `AnalyserNode` as the primary audio intelligence.
- Daisy-to-browser audio capture as a blocker for synth/groovebox work.
- Visual effect tuning as the main roadmap.
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

## Firmware Rules

Firmware should receive compact events and pre-shaped runtime specs. It should
not parse rich project files, decode codecs, resize buffers, log in steady
state, or block inside the audio path.

## Roadmap

The canonical milestone plan lives in `AGENT_MEMORY.md`.

Short form:

1. M1 Playable Host Groovebox.
2. M2 Shared Comms Contract.
3. M3 Pattern Bank And Project Runtime.
4. M4 Spectre Performance Filter Suite.
5. M5 Nexus Voice Expansion.
6. M6 Firmware Groovebox Bridge.
7. M7 Companion Editor And Visual Sync.
