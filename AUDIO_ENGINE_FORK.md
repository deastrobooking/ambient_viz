# Audio-First Fork Plan

This fork should move farther away from the browser/video installation and
prioritize a standalone Rust audio instrument: groovebox, synth engine, sampler,
effects, hardware controls, and performance workflow.

The visualizer can remain useful as an optional companion display, diagnostic
surface, or synchronized projection layer, but it should no longer define the
core architecture.

## North Star

Build a playable hardware groovebox/synth engine around the Daisy:

```text
Pads / encoders / sensors / MIDI / controller MCU
                    |
                    v
              GrooveEvent / MIDI
                    |
                    v
            daisy/crates/dsp::Engine
                    |
        +-----------+-----------+
        |                       |
  Daisy codec line out      optional telemetry
  PA / mixer / headphones   Pi / browser / LEDs
```

The first-class success case is audio coming out of the Daisy line outputs with
low latency, predictable timing, and hands-on control. Video and Pi capture are
integration layers.

## Keep From Ambient Viz

- The existing Daisy workspace split:
  - `daisy/crates/dsp`: shared `no_std` DSP core.
  - `daisy/crates/host`: fast macOS audition path.
  - `daisy/crates/firmware`: embedded Daisy runtime.
- Existing DSP:
  - analog bass drum,
  - closed/open hi-hats,
  - FM stabs,
  - rumble bass,
  - pattern sequencer,
  - sampler,
  - tape/freeze/bloom/reverb/delay/limiter.
- Existing kiosk sensors as optional performance gestures.
- Existing USB CDC/MIDI ideas as control/telemetry transport.

## De-Emphasize

- Browser `AnalyserNode` as the primary audio intelligence.
- Daisy audio capture into Chromium as a blocker for audio work.
- I2S/UAC/WebUSB as first-order product requirements.
- Visual effect tuning as the main roadmap.

Those paths can come back after the instrument is fun and stable.

## Borrow From WolfGang_Rust

Use WolfGang mostly as architecture reference:

- hard realtime contract,
- bounded event buffers,
- transport and quantization concepts,
- session/pattern vocabulary,
- MIDI learn and controller routing concepts,
- project model ideas.

Do not pull the desktop DAW runtime into embedded firmware. Copy ideas, not the
whole app shape.

## Borrow From Nexus12

Use Nexus 12 mostly as synth vocabulary:

- oscillator models,
- fixed polyphonic voice strategy,
- filter families,
- small modulation matrix,
- performance macros,
- master color and transient shaping.

Do not port the plugin surface or NIH-plug assumptions into Daisy firmware.

## Near-Term Architecture

### Shared Control Layer

`daisy/crates/dsp/src/groove.rs` now defines the start of the hardware control
vocabulary:

- `Track`
- `Macro`
- `GrooveEvent`
- `parse_line`

Everything external should translate into one of these before touching the
engine:

```text
keyboard/MIDI/CDC/GPIO/I2C/UI -> GrooveEvent -> Engine::handle_groove_event
```

The standard line protocol is:

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
```

All velocity/macro values are 7-bit `0..127`, so the same commands can be
bridged from MIDI CC/note velocity, CDC serial, a desktop app, or a controller
MCU without inventing per-surface mappings.

### Host First

Develop new instrument behavior in `daisy/crates/host` first:

- faster iteration,
- easy audio output,
- CoreMIDI input,
- file loading,
- tests and logging.

Only move a feature to firmware once it has a bounded realtime shape.

Current host harness:

- starts the sequencer at a fixed 120 BPM / 8 s loop if no timeline sidecar is
  loaded;
- reads the shared groovebox text protocol from stdin;
- applies parsed commands through `Engine::handle_groove_event`;
- keeps old bloom/freeze audition modulation opt-in behind `--test-mod`.

### Firmware Later

Firmware should receive pre-decoded, compact control events. It should not parse
rich project files, decode codecs, resize buffers, log in steady state, or block
inside the audio path.

## Practical Milestones

### M1: Playable Host Groovebox

- Status: first stdin-command pass implemented.
- MIDI pads and `PAD` commands trigger kick/hat/stab.
- `TOGGLE` and `STEP` commands mutate drum/bass steps through `GrooveEvent`.
- `MACRO` commands drive damage, space, tone, levels, and the first Spectre
  dynamic filter band (`filter_cutoff`, `filter_resonance`, `filter_motion`).
- Host prints selected track, sequencer state, and current step after each
  parsed command.

### M2: Pattern Bank

- Multiple patterns in memory.
- Copy/clear/randomize/fill helpers.
- Bass hold/tie editing.
- Pattern change quantized to loop or bar.

### M3: Synth Engine Expansion

- Add one Nexus-inspired oscillator/filter module at a time.
- Add a small fixed modulation matrix.
- Add macro scenes: one knob can morph many engine parameters.
- Keep every new module `no_std`, bounded, and testable.

The intended source map:

- Wolfgang_Rust: session/groovebox architecture, project model, controller
  routing, DrumCanyon/SynthCanyon/SoundCanyon concepts.
- Nexus12: flagship polysynth voice design, oscillator/filter/modulation
  vocabulary, performance UI structure.
- Spectre-Filter: standalone dynamic filter/EQ, analyzer-led filter surface,
  master filter, transient, and color models.

### M4: Hardware Protocol

- Reuse the `groove::parse_line` command vocabulary for CDC and desktop tools.
- Add a binary/MIDI packing only after the text protocol is proven.
- Write a Mac-side sender first.
- Then map the friend's hardware or MCU to the same protocol.

### M5: Standalone Daisy Build

- Daisy boots into a default project/pattern bank.
- Hardware controls reach `Engine::handle_groove_event`.
- Audio comes from codec line out.
- USB/Pi/browser sync is optional, not required for performance.

## Backlog Reframe

Audio-first priorities now outrank visualizer transport work:

1. Host groovebox harness.
2. Pattern/preset control model.
3. Synth/filter/modulation expansion.
4. Hardware control bridge.
5. Firmware standalone groovebox.
6. Optional visual sync and audio capture improvements.

USB capture, I2S, WebUSB, and browser-side analysis are still useful, but they
should not slow down the core instrument path.
