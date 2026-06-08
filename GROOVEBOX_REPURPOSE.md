# Groovebox Repurpose Notes

This note maps the existing `ambient_viz` Daisy/Rust work against the sibling
Rust projects:

- `/Users/randolphchabot/Desktop/iOS_APPS/Nexus12`
- `/Users/randolphchabot/Desktop/iOS_APPS/WolfGang_Rust`

Goal: reuse the best parts of those projects to turn the friend's hardware
platform into a compact groovebox/synth engine rather than starting a new audio
stack.

This fork should be audio-first. The browser visualizer can remain as an
optional synchronized display or diagnostic layer, but the central product is
the instrument: DSP, sequencing, synthesis, sampling, effects, storage, and
hardware control.

## Current Best Base

Use `ambient_viz/daisy` as the hardware runtime and let it drift away from the
video software as needed.

Why:

- `daisy/crates/dsp` is already a shared `no_std` audio core.
- `daisy/crates/host` already runs the same DSP on macOS through CPAL and MIDI.
- `daisy/crates/firmware` already targets Daisy Seed with Embassy, SAI audio,
  USB UAC output, CDC serial, SD-card work, and MIDI plumbing.
- The DSP is already closer to a groovebox than its older README suggests:
  kick, closed/open hats, FM stabs, rumble bass, pattern parser, tempo-aware
  sequencer, sampler, tape degradation, bloom, freeze, reverb, ping-pong delay,
  and limiter are present in `daisy/crates/dsp/src/lib.rs`.

The practical architecture is:

```text
Friend hardware controls / sensors / pads
          |
          v
   MIDI, CDC serial, GPIO, I2C, or UART bridge
          |
          v
   daisy/crates/firmware  ->  daisy/crates/dsp::Engine
          |
          +--> stereo line out to PA
          +--> optional telemetry/sync to Pi, browser, LEDs, or host
```

Line out and hands-on playability come first. USB audio capture, I²S into the
Pi, browser `AnalyserNode` analysis, and visual sync are secondary.

## What To Reuse From WolfGang_Rust

WolfGang is the strongest source for groovebox architecture:

- `docs/REALTIME_AUDIO_CONTRACT.md` gives the right callback discipline:
  pre-allocate, avoid blocking locks, avoid file/network IO, avoid parsing,
  avoid heap allocation, and keep event buffers bounded.
- `docs/ENGINE_DEVELOPER_GUIDE.md` has the right conceptual split:
  transport, MIDI event routing, clip/pattern sources, devices, project/UI
  state, and audio runtime.
- `crates/dsp-core` has small `AudioProcessor` / `MidiInstrument` style traits.
  These could inspire a cleaner device boundary in `daisy/crates/dsp` without
  importing the full desktop engine.
- `crates/midi-core` is especially relevant for controller layers, MIDI learn,
  mapping targets, and feedback models. It has no default serde dependency,
  so it is a plausible candidate for selective copying or adaptation.
- `transport` concepts are useful, but the crate currently depends on
  `project-model`; for Daisy, copy the musical-time ideas rather than pulling
  the crate wholesale.
- DrumCanyon/session/clip concepts are relevant for a future pad/grid
  groovebox, but the full WolfGang runtime is desktop-sized. Treat it as a
  design reference, not the embedded runtime.

Best first borrow: a bounded realtime command/event layer:

```text
Control source -> bounded event queue -> audio block drains events -> Engine params
```

That would let pads, encoders, sensor inputs, and MIDI all speak one internal
language.

## What To Reuse From Nexus12

Nexus 12 is the strongest source for synth and tone-shaping ideas:

- oscillator models: polyBLEP saw/square/pulse, analog saw/pulse, formant,
  terrain/function/karplus-style shapes;
- voice/filter ideas: fixed voice stealing, per-voice filter lanes, SEM,
  ladder, diode, comb;
- modulation ideas: LFO rack, modulation matrix, velocity/key/gate/random
  sources, performance macros;
- master color/transient/filter concepts.

Do not try to port Nexus12 as a plugin. It depends on NIH-plug/egui/plugin
assumptions and is much larger than an embedded groovebox needs.

Best first borrows:

- one or two oscillator/filter algorithms,
- a small fixed-size modulation matrix,
- performance macro patterns such as "one knob moves many engine params."

## What Already Looks Groovebox-Ready

Inside `daisy/crates/dsp`:

- `sequencer.rs`: `.pat` grid parser, 16th-note patterns, chord progression,
  stab tone lane, bass gate/hold lane, tempo curve support.
- `midi_map.rs`: fixed 128-entry CC map with `Param` enum.
- `analog_bass_drum.rs`, `hihat.rs`, `fm_stab.rs`, `bass.rs`: the core drum
  and synth voices.
- `tape/`, `freeze.rs`, `bloom.rs`, `limiter.rs`: performance effects that
  make the box feel alive rather than like a bare drum machine.
- `host/src/main.rs`: fast desktop audition loop through CPAL and CoreMIDI.
- `firmware/src/usb_cdc.rs`: already has CDC position output and inbound MIDI
  byte parsing to a bounded channel.

The missing piece is not "make sound"; it is a control surface/runtime model,
plus a stronger synth/sampler expansion path.

## Proposed Groovebox Shape

### Phase 1: Desktop Groovebox Harness

Keep this on macOS first:

- Add a host control protocol that can receive:
  - pad trigger,
  - step toggle,
  - pattern select,
  - encoder delta,
  - transport play/stop/reset,
  - macro value.
- Extend `daisy/crates/dsp::Param` for groovebox controls:
  - tempo,
  - swing,
  - pattern bank,
  - sequencer enable,
  - kick/hat/stab/bass levels,
  - filter/tape/freeze/bloom macros.
- Add tests around pattern mutation and MIDI/CC mapping.

Outcome: playable groovebox engine on the Mac using the same DSP that will run
on hardware.

### Phase 2: Embedded Control Bridge

Wire the friend's hardware into one of these paths:

- TRS/UART MIDI for classic controller input.
- CDC serial for richer custom messages over USB.
- GPIO/I2C/SPI direct into the Daisy if the control board is physically close
  and electrically simple.
- Pi/ESP32 as a control coprocessor if the hardware has lots of sensors,
  displays, LEDs, or networking.

Recommended default: custom hardware -> small controller MCU -> MIDI or CDC ->
Daisy. Keep Daisy focused on deterministic audio.

### Phase 3: Pattern And Project Storage

The existing `.pat` format is already human-editable. For hardware use:

- store patterns in a fixed bank format,
- convert text `.pat` files to compact binary at build/load time,
- keep runtime mutation in fixed arrays,
- write project saves from the non-audio side only.

WolfGang's `project-model` is a good reference for saved concepts, but Daisy
should use a much smaller fixed-shape model.

### Phase 4: Performance Surface

Minimum satisfying groovebox controls:

- 16 step buttons with LEDs,
- 4 track buttons: kick, hat, stab, bass,
- 4 to 8 encoders with shift pages,
- play/stop/reset,
- pattern select,
- one "damage" macro for tape/freeze/glitch,
- one "space" macro for reverb/delay/bloom,
- one "tone" macro per selected voice.

The existing kiosk sensors can become performance gestures:

- distance -> bloom or tape clarity,
- breath -> freeze/glitch punch-in,
- touch mask -> scene/pattern/macro toggles,
- motion -> wake/idle or generative fills.

## Key Engineering Constraint

Do not merge the desktop DAW runtime into the embedded path.

The embedded audio callback should see:

- fixed-size patterns,
- fixed-size event queues,
- fixed-size voices,
- pre-allocated effect buffers,
- no file parsing,
- no codec decode,
- no heap growth,
- no blocking locks.

That matches WolfGang's realtime contract and the current Daisy architecture.

## Immediate Next Build Step

The most useful next code step is a small `daisy/crates/dsp` groovebox control
API:

```rust
pub enum GrooveEvent {
    TransportPlay(bool),
    TransportReset,
    SelectTrack(Track),
    ToggleStep { track: Track, step: u8 },
    SetStepVelocity { track: Track, step: u8, velocity: f32 },
    SetMacro { macro_id: u8, value: f32 },
    Pad { note: u8, velocity: f32 },
}
```

Then both host and firmware can feed the same event type:

```text
MIDI/CDC/GPIO -> GrooveEvent -> Engine::handle_groove_event -> audio output
```

That is the bridge between the friend's hardware and the existing Rust audio
work.
