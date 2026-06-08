# Groovebox Repurpose Research

This note captures the condensed research pass across the local sibling
projects:

- `/Users/randolphchabot/Desktop/iOS_APPS/WolfGang_Rust`
- `/Users/randolphchabot/Desktop/iOS_APPS/Nexus12`
- `/Users/randolphchabot/Desktop/iOS_APPS/Spectre-Filter`
- `/Users/randolphchabot/Desktop/iOS_APPS/Wolfgang_iOS`

## Conclusion

Use `ambient_viz/daisy` as the hardware runtime and let it drift away from the
video software. It already has the right foundation: `no_std` DSP, macOS host
audition, Daisy firmware target, sequencer, drums, stabs, bass, sampler,
performance effects, MIDI/CDC plumbing, and codec/I/O work.

The missing product layer is not sound generation; it is a hardware-friendly
groovebox runtime: shared controls, pattern banks, project state, macro scenes,
and firmware bridge.

## Hardware Shape

Recommended default:

```text
friend hardware / pads / sensors / LEDs
          |
          v
small controller MCU or direct Daisy peripheral
          |
          v
MIDI, CDC serial, UART, GPIO, I2C, or SPI
          |
          v
GrooveEvent -> daisy/crates/dsp::Engine -> Daisy line out
```

Keep Daisy focused on deterministic audio. Use a controller MCU or host process
for displays, dense scanning, networking, rich logging, or UI translation when
that complexity would disturb the audio path.

## Reusable Ideas

Wolfgang_Rust and Wolfgang_iOS confirm the control/runtime split:

- UI/control state owns editing and persistence.
- The render/audio path consumes snapshots or compact commands.
- Transport, step edits, automation, and MIDI routing must stay bounded.
- Controller feedback belongs in a separate mapping layer.

Nexus12 confirms the synth target:

- fixed voices,
- bounded modulation,
- stable parameter IDs,
- strong performance macros,
- page-based editor surface.

Spectre confirms the filter target:

- eight retained dynamic bands,
- channel-aware processing,
- envelope motion,
- master filter/color/transient path,
- analyzer data generated for UI, not required for audio.

## Current Best Next Use

Keep building through the shared `GrooveEvent` path:

```text
MIDI/CDC/GPIO/UI -> GrooveEvent -> Engine::handle_groove_event
```

Immediate hardware-facing priorities:

- richer host state feedback;
- selected-band filter controls;
- pattern bank operations;
- compact serial/MIDI encoding after the text protocol feels good;
- firmware bridge into the same event path.

The detailed milestone plan lives in `AGENT_MEMORY.md`.
