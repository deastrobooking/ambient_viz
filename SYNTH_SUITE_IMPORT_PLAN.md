# Donor Import Boundaries

This repo owns the standalone instrument runtime. Wolfgang, Nexus 12, and
Spectre are references and selective donor sources, not runtimes to merge whole.

The rule: every imported idea becomes a small, tested, realtime-bounded module
behind `daisy/crates/dsp::Engine` or the shared `GrooveEvent` protocol.

## Never Import Into Firmware

- NIH-plug, CLAP/VST3, egui, or plugin parameter layers.
- Desktop DAW/session app runtime.
- GUI graph/analyzer state.
- File IO, allocation, parsing, or blocking locks in audio processing.
- Unbounded project/preset state inside `dsp`.

## Wolfgang_Rust

Use as the groovebox/workstation architecture reference.

Borrow:

- realtime callback contract;
- transport and quantization;
- session/pattern/project concepts;
- MIDI learn, controller layers, feedback mapping;
- DrumCanyon/SynthCanyon/SoundCanyon concepts reduced to embedded endpoints.

Avoid:

- full DAW mixer/session runtime;
- desktop app shell;
- patch graph execution before the embedded control model is stable.

## Nexus12

Use as the flagship synth reference.

Borrow:

- fixed-size polyphony and deterministic voice stealing;
- oscillator families;
- per-voice filter families;
- small modulation matrix;
- performance LFOs and macro scenes;
- editor/page vocabulary for future companion UI.

Avoid:

- plugin/editor assumptions;
- host automation parameter layer;
- unbounded preset or UI state in `dsp`.

## Spectre-Filter

Use as the standalone filter/effects reference.

Borrow:

- dynamic filter/EQ behavior;
- channel modes;
- master filter, transient, and color models;
- envelope detectors and performance LFOs;
- analyzer snapshots for host/editor display.

Avoid:

- DAW sidechain bus assumptions;
- GUI graph state in processing;
- analyzer allocation in realtime.

## Current Import Status

Landed:

- shared `GrooveEvent` and text protocol;
- host stdin groovebox harness;
- default-bypassed Spectre dynamic rack and master filter in `Engine`;
- macro ids 7-9 for `filter_cutoff`, `filter_resonance`, `filter_motion`;
- selected-band `BAND` and explicit/selected `FILTER` commands;
- finite-output tests for the Spectre filter path.

Next donor work is tracked in `AGENT_MEMORY.md` milestones M3-M5.

## Acceptance Gate

Every imported feature needs:

- one narrow host-side control path;
- finite-output DSP tests where applicable;
- no process-time allocation by design;
- a clear firmware-control story;
- docs updated in `AGENT_MEMORY.md` and `BACKLOG.md`.
