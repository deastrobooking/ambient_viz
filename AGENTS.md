# Agent Guide

This repository is in an audio-first fork transition.

Future agents should not assume the browser visualizer is the product center.
The definitive product direction is now a standalone Rust audio performance
instrument: Daisy-based groovebox, synth engine, sampler, effects, hardware
controls, and shared comms.

## Read First

Before planning or coding, read:

1. `AGENT_MEMORY.md` — compact current-state handoff and milestone plan.
2. `AUDIO_ENGINE_FORK.md` — north star and architecture summary.
3. `SYNTH_SUITE_IMPORT_PLAN.md` — Wolfgang/Nexus/Spectre import boundaries.
4. `daisy/README.md` — current Daisy workspace framing and workflow.
5. `BACKLOG.md` — prioritized permanent task list.

Older visualizer/kiosk docs are still valid for exhibit work, but they are no
longer the main roadmap for this fork.

## Architecture Rules

- `daisy/crates/dsp` owns the shared `no_std` audio engine.
- `daisy/crates/host` is the fast macOS audition/control harness.
- `daisy/crates/firmware` is the embedded runtime.
- Browser/Pi/visualizer integration is optional telemetry or companion output.
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
