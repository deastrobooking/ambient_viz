# Agent Memory

Last updated: 2026-06-08.

## Direction

This repository is now a full rebuild fork into a separate audio product. Treat
`daisy/` as the product core: a standalone Daisy groovebox/synth engine with
sampler, effects, hardware controls, shared comms, project runtime, and codec
line out.

The browser visualizer and Pi stack are optional companion layers for projection,
telemetry, diagnostics, or legacy kiosk use. They are not the product center and
must not block audio-engine, groovebox, firmware, or hardware-control progress.

## Canonical Docs

Read in this order:

1. `AGENT_MEMORY.md` — current state, donor research, milestone plan.
2. `AGENTS.md` — standing agent rules for this fork.
3. `AUDIO_ENGINE_FORK.md` — product architecture/north star.
4. `SYNTH_SUITE_IMPORT_PLAN.md` — donor import boundaries.
5. `daisy/README.md` — workspace/hardware notes.
6. `PI4_AUDIO_TEST_DEPLOYMENT.md` — Pi 4 companion deployment/testing guide.
7. `BACKLOG.md` — permanent task list.

Legacy visualizer docs (`DAISY_I2S_SETUP.md`, `INSTALL_DAY.md`,
`SENSOR_MAPPING.md`, `PI_KIOSK_BRINGUP.md`) are still useful for exhibit work,
but they are no longer product or architecture authority.

Use `PI4_AUDIO_TEST_DEPLOYMENT.md` when testing the current audio fork on a Pi
4. It defines the Pi as a companion for mock SSE, sensors, Daisy CDC
song-position/control, and visual sync. Audio acceptance remains Daisy codec
line out, not Pi/browser USB capture.

## Current Implementation

Shared control lives in `daisy/crates/dsp/src/groove.rs`:

- `Track`: kick, closed hat, open hat, stab, bass.
- `Macro`: damage, space, tone, lane levels, Spectre filter cutoff/resonance/motion.
- `FilterParam`: cutoff, resonance, motion.
- `GrooveEvent`: transport, track select, step edit, macro, pad trigger, and
  Spectre dynamic filter band selection/editing.
- `parse_line`: compact text protocol for host/CDC/MCU tools.

Protocol examples:

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
FILTER 4 motion 96
```

All velocity/macro values are 7-bit `0..127`; keep that mapping stable so MIDI,
CDC serial, OSC/WebSocket bridges, controller MCUs, and future desktop editors
can share one vocabulary.

`daisy/crates/dsp/src/lib.rs` now exposes `Engine::handle_groove_event` and
routes groove events into transport, selected track, sequencer step edits, pad
triggers, pattern-bank operations, lane levels, damage/space/tone macros,
selected Spectre band, and Spectre filter parameter edits.

`daisy/crates/host/src/main.rs` starts a fixed 120 BPM loop when no sidecar is
loaded, reads the line protocol from stdin, applies events through the shared
engine path, and leaves old exhibit bloom/freeze modulation behind `--test-mod`.

`daisy/crates/dsp/src/spectre_filter.rs` contains the first standalone Spectre
port:

- master filter models: Off, Clean LP, Ladder12, Ladder24, Diode, SEM Morph;
- eight dynamic biquad bands;
- band modes: Bell, LowShelf, HighShelf, LowCut, HighCut, Notch, BandPass;
- channel modes: Stereo, Mid, Side, Left, Right;
- envelope-followed dynamic gain and cutoff sweep;
- default-bypassed engine dynamic rack and master insert before tape.

Macro ids 7-9 steer the currently selected dynamic band:

- `filter_cutoff`
- `filter_resonance`
- `filter_motion`

## Donor Research

Use sibling projects as references and small donor modules, not as whole
runtimes.

### WolfGang_Rust

Reference path: `/Users/randolphchabot/Desktop/iOS_APPS/WolfGang_Rust`

Best ideas to borrow:

- hard realtime contract: no allocation/blocking/file/network work in audio;
- transport, quantization, session grid, clip/pattern launch;
- project/preset state and save/load separation from runtime specs;
- MIDI learn, controller routing, and feedback mapping;
- DrumCanyon/SynthCanyon/SoundCanyon concepts as compact embedded endpoints.

Avoid importing the DAW app shell, full session mixer, desktop graph runtime, or
project-model dependency tree into firmware.

### Nexus12

Reference path: `/Users/randolphchabot/Desktop/iOS_APPS/Nexus12`

Best ideas to borrow:

- fixed-size polyphonic synth engine with deterministic voice stealing;
- oscillator vocabulary: polyBLEP saw/square/pulse, analog shapes, formant,
  terrain/function, Karplus;
- filter lanes: ladder, SEM, diode, comb, clean LP;
- bounded modulation matrix and performance LFOs;
- macro scenes where one control morphs multiple parameters;
- page structure for a future desktop/editor surface.

Avoid NIH-plug, egui/plugin parameter assumptions, and unbounded UI/preset state
inside `dsp`.

### Spectre-Filter

Reference path: `/Users/randolphchabot/Desktop/iOS_APPS/Spectre-Filter`

Best ideas to borrow:

- dynamic filter/EQ band behavior and channel modes;
- master filter, transient shaping, color models;
- envelope detectors and performance LFOs;
- analyzer snapshot path, kept host-side only.

Avoid CLAP/VST3 sidechain bus assumptions, GUI graph state, and analyzer
allocation in realtime processing.

## Milestone Plan

### M0: Memory And Scope Cleanup

Status: completed in this working tree.

- Condense fork docs so `AGENT_MEMORY.md` is canonical.
- Keep `AUDIO_ENGINE_FORK.md`, `SYNTH_SUITE_IMPORT_PLAN.md`, and
  `GROOVEBOX_REPURPOSE.md` short and non-overlapping.
- Keep `BACKLOG.md` aligned with the milestones below.

Acceptance: future agents can understand current scope in under five minutes
without reading the old visualizer handoff first.

### M1: Playable Host Groovebox

Status: partially implemented.

- Current: host stdin protocol, fixed tempo loop, pad triggers, step edits,
  selected track, transport, lane levels, damage/space/tone/filter macros.
- Current: `HELP` and `STATE` host commands print command help and a performance
  snapshot with transport, selected track, step position, selected step value,
  macro values, Spectre band 1, and master-filter state.
- Next: keyboard shortcuts or a small TUI so the host is playable without
  typing full commands.

Acceptance: on macOS, the user can perform a basic pattern, trigger pads, edit
steps, and sweep filter/damage/space macros against the same DSP used by Daisy.

### M2: Shared Comms Contract

Status: partially implemented.

- Current: macro ids and command spellings exist for transport, pads, steps,
  lane selection, lane levels, and Spectre filter motion.
- Current: `BAND <1..8>` selects a dynamic filter band.
- Current: `FILTER <param> <0..127>` edits the selected band, and
  `FILTER <1..8> <param> <0..127>` edits an explicit band.
- Add a compact event encoder/decoder for CDC/MIDI/UART only after the text
  protocol feels right.
- Add tests for every command and every macro id.

Acceptance: host, firmware, and future controller MCU can all translate into
`GrooveEvent` without bespoke engine calls.

### M3: Pattern Bank And Project Runtime

Status: partially implemented.

- Current: `PatternSnapshot` copies fixed sequencer pattern state without
  realtime allocation.
- Current: `PatternBank` provides 8 fixed slots with capture/load/copy/clear,
  fill, and deterministic randomize helpers.
- Current: `Engine` owns a pattern bank and routes `PATTERN`, `CAPTURE`,
  `PCOPY`, `PCLEAR`, `PFILL`, and `PRAND` through `GrooveEvent`.
- Current: live and slot bass hold/tie editing is available through `BASS` and
  `PBASS`.
- Current: `PATTERN <slot>` queues a pattern load and applies it at step 0 when
  selected mid-loop; selecting at step 0 loads immediately.
- Next: minimal project snapshot format generated outside the audio path.

Acceptance: multiple patterns can be edited and switched without allocation or
timing surprises in audio processing.

### M4: Spectre Performance Filter Suite

Status: partially implemented.

- Current: default-bypassed dynamic rack and master insert landed.
- Current: selected-band protocol/editing landed via `BAND` and `FILTER`.
- Current: host `STATE` displays active band frequency, Q, dynamic amount, and
  sweep.
- Current: `DynamicFilter::envelope_values`, `Engine::spectre_dynamic_envelopes`,
  and host `STATE` expose selected-band envelope activity for performance
  metering.
- Next: port transient shaping and master color models as standalone effects.
- Keep analyzer data host/editor-side.

Acceptance: the fork has a musically useful standalone filter/effects section
that can be played from hardware macros without plugin dependencies.

### M5: Nexus Voice Expansion

Status: planning.

- Add one oscillator family at a time with finite-output tests.
- Add one filter family at a time with bounded state.
- Add small modulation matrix: LFO, velocity, key, gate, random, envelope.
- Add macro scenes after the destination set is stable.

Acceptance: a fixed-size synth voice can run in `dsp` with no process-time
allocation and can be controlled through the shared macro/mod system.

### M6: Firmware Groovebox Bridge

Status: planning.

- Route CDC/MIDI/UART/hardware controls into `GrooveEvent`.
- Keep parsing, storage, logging, and project mutation outside the audio path.
- Boot into a default project/pattern bank.
- Prioritize Daisy codec line out over browser/USB capture.

Acceptance: the Daisy can run standalone with hardware controls and line output;
Pi/browser sync is optional.

### M7: Companion Editor And Visual Sync

Status: deferred.

- Desktop/editor UI can borrow Nexus/Spectre visual language.
- Visualizer can receive telemetry or clock/pattern state.
- Browser analysis/capture work resumes only after the instrument path is fun
  and stable.

Acceptance: visual tools enhance the instrument without defining its runtime.

## Next Milestone Planning

Recommended next slices:

1. **Finish M3 live pattern behavior**
   Add a minimal project snapshot format outside the audio path.

2. **Advance M4 filter suite**
   Port Spectre transient and master color models as standalone no-alloc
   effects, then add host/editor analyzer snapshots.

3. **Prepare M5 synth expansion**
   Choose the first Nexus-inspired oscillator/filter pair and define its fixed
   voice/state budget before coding.

## Verification

Last successful checks:

```sh
cd daisy
/Users/randolphchabot/.cargo/bin/cargo test -p dsp
/Users/randolphchabot/.cargo/bin/cargo check -p host
```

Results:

- `dsp`: 61 tests passed after adding Spectre dynamic envelope metering.
- `host`: check passed.

Known warnings:

- vendored `infinitedsp-core` deprecated `wide::f32x4::sign_bit`;
- old sequencer test helper unused `loop_s`;
- `block v0.1.6` future incompat warning through host dependencies.
