# Agent Guide

This repository is now a separate audio-instrument product fork.

Future agents should not assume the browser visualizer is the product center.
The definitive product direction is a standalone Rust audio performance
instrument: Daisy-based groovebox, synth engine, sampler, effects, pattern
runtime, hardware controls, and shared comms.

## Read First

Before planning or coding, read:

1. `AGENT_MEMORY.md` — compact current-state handoff and milestone plan.
2. `AUDIO_ENGINE_FORK.md` — north star and architecture summary.
3. `SYNTH_SUITE_IMPORT_PLAN.md` — Wolfgang/Nexus/Spectre import boundaries.
4. `daisy/README.md` — current Daisy workspace framing and workflow.
5. `PI4_AUDIO_TEST_DEPLOYMENT.md` — Pi 4 companion deployment/testing.
6. `BACKLOG.md` — prioritized permanent task list.

Older visualizer/kiosk docs are still valid for exhibit work, but they are
legacy companion docs, not the product roadmap.

## Architecture Rules

- `daisy/crates/dsp` owns the shared `no_std` audio engine.
- `daisy/crates/host` is the fast macOS audition/control harness.
- `daisy/crates/firmware` is the embedded runtime.
- Browser/Pi/visualizer integration is optional telemetry or companion output.
- Pi 4 deployment for the audio fork is documented in
  `PI4_AUDIO_TEST_DEPLOYMENT.md`; use `PI_KIOSK_BRINGUP.md` only for full
  legacy sensor/exhibit bringup.
- Codec/line-out audio and hands-on playability come first.

All external control surfaces should target the shared control vocabulary:

```text
keyboard/MIDI/CDC/GPIO/I2C/UI -> GrooveEvent -> Engine::handle_groove_event
```

Line protocol:

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

## Donor Project Boundaries

- `WolfGang_Rust`: borrow groovebox/DAW architecture, transport, controller
  routing, pattern/session/project ideas. Do not import the full desktop DAW
  runtime into embedded firmware.
- `Nexus12`: borrow synth voice, oscillator/filter/modulation, performance UI
  and macro ideas. Do not import NIH-plug or plugin editor assumptions into
  `dsp`.
- `Spectre-Filter`: borrow standalone filter/EQ, master filter, envelope,
  transient/color, and analyzer ideas. Keep analyzer/UI host-side; keep `dsp`
  bounded and realtime-safe.

## Realtime Rules

For `daisy/crates/dsp` and firmware audio paths:

- no file IO,
- no network IO,
- no blocking locks,
- no logging in steady-state processing,
- no codec decode,
- no unbounded graph traversal,
- no process-time allocation or buffer growth,
- fixed/bounded event buffers and voices,
- finite-output tests for DSP imports.

## Verification

Common checks:

```sh
cd daisy
/Users/randolphchabot/.cargo/bin/cargo test -p dsp
/Users/randolphchabot/.cargo/bin/cargo check -p host
```

Known current warnings:

- vendored `infinitedsp-core` uses deprecated `wide::f32x4::sign_bit`;
- a sequencer test helper has an unused `loop_s` argument.
