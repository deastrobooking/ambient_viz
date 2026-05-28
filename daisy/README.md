# ambient-viz-daisy

Cargo workspace for the ambient visualizer's audio coprocessor:

| Crate | Type | What it is |
|---|---|---|
| `crates/dsp` | `no_std` library | Audio + MIDI core. Sampler, mixer, voice allocation. Same code on both targets. |
| `crates/firmware` | embedded binary (thumbv7em) | Daisy Seed firmware. Embassy + SAI + USB UAC + UART-MIDI + `dsp`. |
| `crates/host` | std binary (macOS) | Local dev host. CoreAudio + CoreMIDI + `dsp`. Lets you iterate on the audio logic without reflashing. |

End goal: physical MIDI controller (TRS 3.5mm → UART) drives a sampler/mixer
on the Daisy. Output goes both to the Daisy codec (→ PA) and to the Pi over
USB Audio Class (→ visualizer).

## Dev workflow

```bash
# Mac iteration — edit dsp/, hear the change in ~3-5s
cargo run -p host --release

# Flash to Daisy (with debug probe)
cargo flash

# Flash to Daisy (DFU, no probe)
cargo bin
# hold BOOT, tap RESET, release BOOT
dfu-util -a 0 -s 0x08000000:leave -D target/firmware.bin
```

`cargo flash` and `cargo bin` are aliases defined in `.cargo/config.toml`
that pass `-p firmware --target thumbv7em-none-eabihf --release`. The
workspace's `default-members` excludes `firmware`, so a bare `cargo build`
from the root builds only the Mac-buildable crates and won't fail on
firmware's thumb target.

## Prerequisites

```bash
# Toolchain (auto-installed from rust-toolchain.toml on first build)
rustup target add thumbv7em-none-eabihf

# Flashing — pick one path:

# (A) probe-rs — requires a debug probe (ST-Link, DAPLink, etc.)
cargo install probe-rs-tools --locked

# (B) DFU — works with stock Daisy, no extra hardware
brew install dfu-util
cargo install cargo-binutils
```

## Hardware target

This workspace assumes the **original Daisy Seed** (Rev 6, AK4556 codec,
64 MB SDRAM, SD card adapter wired to SDMMC1). For Seed 1.1 / 1.2 / Patch SM,
change the `daisy-embassy` feature flag in `crates/firmware/Cargo.toml`:

```toml
features = ["seed_1_1"]   # or seed_1_2, patch_sm
```

## Architecture

```
                         ┌─────────────────┐
                         │  crates/dsp     │     no_std, no allocator
                         │  Engine::process│     buffer-size + sr agnostic
                         │  Engine::handle_midi   stereo interleaved f32
                         └────┬───────┬────┘
                              │       │
                ┌─────────────┘       └────────────────┐
                │                                      │
        ┌───────▼────────┐                    ┌────────▼────────┐
        │ crates/host    │                    │ crates/firmware │
        │  cpal output   │                    │  embassy SAI    │
        │  midir input   │                    │  USB UAC source │
        │                │                    │  USART MIDI in  │
        └───────┬────────┘                    └────────┬────────┘
                │                                      │
       macOS CoreAudio                       Codec out → PA
       macOS CoreMIDI                        USB out → Pi (visualizer)
                                             UART in ← TRS MIDI adapter
```

Why this works:
- `dsp` is `no_std` so it compiles unchanged into both targets.
- All I/O (audio, MIDI, USB) lives in the host-specific crate. The `dsp`
  crate never directly touches a peripheral or a `std` type.
- `MidiMessage` will be a small enum decoded by each host from its own
  transport. (Not yet implemented — currently `dsp` is a sine wave stub.)
- Buffer sizes differ (~512 frames on cpal, ~48 on embassy SAI) — `Engine::process`
  is block-size agnostic so this is transparent.

## Sample storage

The 18 MB MP3 doesn't fit in QSPI flash (8 MB) and its decoded form
doesn't fit in SDRAM (64 MB). SD card via SDMMC1 is the destination.
Likely path: bake the file as i16 PCM mono onto the SD card (~50 MB at
22 kHz mono, no decode CPU at runtime), stream into a ring buffer from
a low-priority embassy task. On the Mac, host reads the same WAV from disk.

Not yet implemented — `dsp` doesn't know about samples at all in this
revision. When we add a sampler, samples will reach `dsp` as `&[i16]`
slices that hosts source however they like.

## Project layout

```
daisy/
├── Cargo.toml                          # [workspace] + patch.crates-io
├── .cargo/config.toml                  # target.thumbv7em block + aliases
├── rust-toolchain.toml                 # stable + thumbv7em + llvm-tools
├── README.md
└── crates/
    ├── dsp/
    │   ├── Cargo.toml
    │   └── src/lib.rs                  # Engine + future MIDI types
    ├── firmware/
    │   ├── Cargo.toml
    │   ├── memory.x                    # STM32H750IB linker layout
    │   ├── build.rs                    # copies memory.x into OUT_DIR
    │   └── src/main.rs                 # embassy entry point (blinky)
    └── host/
        ├── Cargo.toml
        └── src/main.rs                 # cpal sine-wave output
```

## Roadmap

1. **Workspace + sine wave** ← you are here. `cargo run -p host` plays a sine.
2. **MIDI input on host** — `midir` enumerates CoreMIDI ports, feeds `Engine::handle_midi`.
3. **Sampler in dsp** — voice allocation, ADSR, sample playback from `&[i16]`.
4. **Daisy SAI passthrough** — wire `Engine::process` into the audio callback.
5. **Daisy UART-MIDI** — 31.25 kbaud USART RX, decode → `Engine::handle_midi`.
6. **Daisy USB UAC source** — Pi captures audio over USB.
7. **SD card sample storage** — `embedded-sdmmc` + ring buffer.
```
