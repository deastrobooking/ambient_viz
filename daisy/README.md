# ambient-viz-daisy

Cargo workspace for the fork's audio-first Daisy engine:

| Crate             | Type                        | What it is                                                                                            |
| ----------------- | --------------------------- | ----------------------------------------------------------------------------------------------------- |
| `crates/dsp`      | `no_std` library            | Groovebox/synth core: sampler, drums, FM stabs, bass, sequencer, tape, freeze, bloom, MIDI, controls. |
| `crates/firmware` | embedded binary (thumbv7em) | Daisy Seed firmware. Embassy + SAI audio, USB CDC/UAC support, UART-MIDI, SD-card work, and `dsp`.    |
| `crates/host`     | std binary (macOS)          | Local dev host. CoreAudio + CoreMIDI + `dsp`. Lets you iterate on audio logic without reflashing.     |

Fork direction: the Daisy is the instrument. Video/visualizer integration is
optional downstream telemetry, not the center of the architecture. The primary
goal is a playable hardware groovebox/synth engine controlled by pads, encoders,
MIDI, CDC serial, sensors, or a small companion MCU.

Audio output should be excellent over the Daisy codec/line out first. USB audio,
I²S, Pi capture, and browser analysis are secondary integration paths.

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

### Production build (no debug UART)

The dev builds emit plain-text diagnostics over USART3 (D2): a boot log, a
1 Hz audio-health heartbeat, a ~5 s STM32 die-temperature readout, and panic
messages. These are all gated behind the `debug-uart` cargo feature (on by
default). The shipping kiosk firmware strips them with `--no-default-features`
— USART3 is never brought up, every `dbg_uart!` compiles to nothing, and the
panic handler just halts:

```bash
# Production DFU image -> target/firmware-prod.bin (no UART debug traffic)
cargo bin-prod
# hold BOOT, tap RESET, release BOOT
dfu-util -a 0 -s 0x08000000:leave -D target/firmware-prod.bin

# Or flash directly with a probe:
cargo flash-prod
```

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

This workspace assumes the **original Daisy Seed** (Rev 7, PCM3060 codec,
64 MB SDRAM, SD card adapter wired to SPI). For Seed 1.1 / 1.2 / Patch SM,
change the `daisy-embassy` feature flag in `crates/firmware/Cargo.toml`:

```toml
features = ["seed_1_2"]   # or seed_1_1, patch_sm
```

## Architecture

```
                         ┌─────────────────┐
                         │  crates/dsp     │     no_std audio engine
                         │  Engine::process│     stereo interleaved f32
                         │  MIDI + GrooveEvent controls
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
       macOS CoreMIDI/CDC                    UART/CDC/MIDI controls
       fast audition                         optional USB/Pi/visualizer sync
```

Why this works:

- `dsp` is `no_std` so it compiles unchanged into both targets.
- All I/O (audio, MIDI, USB) lives in the host-specific crate. The `dsp`
  crate never directly touches a peripheral or a `std` type.
- `MidiMessage` and `GrooveEvent` are small control types decoded by each host
  from its own transport.
- Buffer sizes differ (~512 frames on cpal, ~48 on embassy SAI) — `Engine::process`
  is block-size agnostic so this is transparent.

## Sample storage

The 18 MB MP3 doesn't fit in QSPI flash (8 MB) and its decoded form
doesn't fit in SDRAM (64 MB). SD card via SPI is the destination.
Likely path: bake the file as i16 PCM mono onto the SD card (~50 MB at
22 kHz mono, no decode CPU at runtime), stream into a ring buffer from
a low-priority embassy task. On the Mac, host reads the same WAV from disk.

The host path already decodes file-backed samples into stereo f32 and feeds the
sampler. Firmware sample storage remains the embedded problem: SD streaming or
pre-baked PCM banks, prepared outside the realtime audio callback.

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

1. **Host groovebox harness** — map keyboard/MIDI/serial controls to
   `GrooveEvent` so patterns, pads, and macros are playable on macOS.
2. **Control protocol** — define a small CDC/MIDI-friendly command vocabulary
   for pads, steps, macros, transport, and selected track.
3. **Pattern editing** — add bass ties/holds, copy/clear, pattern banks, and
   realtime-safe mutation helpers.
4. **Synth expansion** — selectively port Nexus 12/WolfGang oscillator, filter,
   modulation, and macro ideas into small fixed-size `no_std` modules.
5. **Hardware bridge** — map the friend's controller hardware through MIDI,
   CDC serial, or a small MCU into `GrooveEvent`.
6. **Firmware groovebox build** — make the Daisy standalone: codec line out,
   hardware control input, project/pattern/sample storage, and bounded realtime
   audio.
7. **Optional visual sync** — send audio position/features to Pi/browser only
   after the instrument works as a standalone box.

```

```
