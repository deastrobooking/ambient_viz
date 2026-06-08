# Wolfgang / Nexus / Spectre Import Plan

This fork is the definitive audio performance and synth software. The other
Rust projects are source references and potential donor modules, but this repo
owns the standalone instrument runtime.

## Rule Of The Fork

Keep the runtime centered on `daisy/crates/dsp::Engine` and its shared control
protocol. Features from Wolfgang, Nexus 12, and Spectre should enter through
small, tested, realtime-bounded modules.

Do not import desktop/plugin assumptions into the embedded path:

- no NIH-plug dependency in Daisy firmware,
- no egui/UI state in `dsp`,
- no allocation or file IO in audio processing,
- no DAW graph/runtime dependency in the hardware engine.

## Standard Comms

All surfaces should target `GrooveEvent`.

Line protocol:

```text
PLAY 1
STOP
RESET
TRACK kick
PAD 36 127
TOGGLE kick 0
STEP bass 4 96
MACRO damage 64
```

The protocol intentionally uses 7-bit values so it maps cleanly to MIDI,
encoders, CDC serial, OSC/WebSocket bridges, or a controller MCU.

## Wolfgang_Rust Feature Targets

Use Wolfgang as the groovebox and workstation reference.

Priority imports:

1. Transport/quantization concepts.
2. Pattern/session model.
3. MIDI learn, controller layers, and feedback mapping.
4. DrumCanyon pad model: pad source, choke, bus, kit, sequence.
5. SynthCanyon/SoundCanyon rack vocabulary, but folded into bounded endpoints.
6. Project/preset model, reduced to fixed-shape embedded banks.

Avoid importing:

- desktop app shell,
- full DAW mixer/session runtime,
- patch graph execution before the embedded endpoint model is stable.

## Nexus12 Feature Targets

Use Nexus 12 as the flagship synth reference.

Priority imports:

1. Oscillator vocabulary: polyBLEP saw/square/pulse, analog saw/pulse, formant,
   terrain/function, Karplus-style modes.
2. Fixed polyphonic voice engine with deterministic stealing.
3. Per-voice filter lanes: ladder, SEM, diode, comb, clean LP.
4. Fixed modulation matrix: LFO, velocity, key, gate, random, envelope sources.
5. Performance macros and morph scenes.
6. UI/page structure for the eventual desktop/editor surface.

Avoid importing:

- NIH-plug parameter layer,
- plugin editor assumptions,
- unbounded preset/UI state into `dsp`.

## Spectre-Filter Feature Targets

Use Spectre as the standalone filter/effects reference.

Priority imports:

1. Eight-band dynamic filter/EQ as a standalone `dsp` effect.
2. Master filter models: Clean LP, Ladder12, Ladder24, Diode, SEM Morph.
3. Envelope follower detectors and dynamic band gain/cutoff movement.
4. Performance LFOs for filter and macro motion.
5. Master transients and master color models.
6. Analyzer data path for a host/editor, not for the embedded audio callback.

Avoid importing:

- CLAP/VST3 aux bus assumptions,
- GUI graph state into processing,
- analyzer allocation in realtime.

## Implementation Order

### 1. Comms And Host Harness

- Finish `groove::parse_line`.
- Add host stdin/UDP/serial-style command ingestion.
- Print state snapshots for pattern position, selected track, and macros.

### 2. Standalone Spectre Filter Core

- Status: first master-filter pass implemented in `daisy/crates/dsp/src/spectre_filter.rs`.
- Landed:
  - master filter models: Off, Clean LP, Ladder12, Ladder24, Diode, SEM Morph;
  - envelope follower;
  - eight-band dynamic biquad core;
  - channel modes: Stereo, Mid, Side, Left, Right;
  - envelope-followed dynamic gain/cutoff movement;
  - default-bypassed `Engine` dynamic rack and master insert before tape;
  - Tone macro can open the filter as a performance color path;
  - `filter_cutoff`, `filter_resonance`, and `filter_motion` macros steer the
    first dynamic band through the shared protocol;
  - finite-output tests.
- Next add selected-band protocol controls, transient/color models, and host
  analyzer state.
- Keep analyzer/UI state host-side only.

### 3. Nexus Voice Expansion

- Add one oscillator at a time.
- Add one filter family at a time.
- Keep every voice and filter allocation-free after construction.
- Add no-NaN/no-infinite tests across MIDI range.

### 4. Wolfgang Groovebox Model

- Pattern banks.
- Quantized scene/pattern launch.
- Controller mapping and feedback.
- Reduced project save/load format.

### 5. Editor/UI Surface

- Desktop editor can borrow Nexus/Spectre visual ideas.
- Hardware stays command/protocol driven.
- The UI edits project/control state; the audio engine consumes bounded runtime
  specs and `GrooveEvent`s.

## Acceptance Gate

Every imported feature needs:

- a narrow host-side test,
- finite-output coverage for DSP,
- no process-time allocation by design,
- one host harness control path,
- a clear firmware story before it is considered hardware-ready.
