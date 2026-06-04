# Future improvement: run firmware from QSPI flash via the Daisy bootloader

**Status:** not started — documented as a future improvement.
**Why it matters:** the firmware is bumping the STM32H750's tiny **128 KB
internal flash**. The Daisy Seed has an **8 MB external QSPI flash** chip that we
are not using; running from it removes the flash ceiling entirely (64× the room).

## The three memories on the Daisy Seed (don't confuse them)

| Memory | Size | Address | What it is | Today |
|---|---|---|---|---|
| Internal flash | **128 KB** | `0x08000000` | On-die flash in the STM32H750 | **What we run from now — the ceiling we keep hitting** |
| External QSPI flash | **8 MB** | `0x90000000` | Separate NOR chip on the Daisy board | Declared in `memory.x` as `QSPIFLASH`, but unused |
| SDRAM | **64 MB** | `0xC0000000` | External DRAM (volatile) | Inited at boot; the heap is in AXI-SRAM, not here (see note) |

The "8 MB external flash" and "64 MB SDRAM" in the Daisy spec sheet are *both*
external chips, *both* different from the 128 KB internal flash. The H750 is the
value-line die with only 128 KB on-chip flash (its sibling H753 has 2 MB).

## Current flash budget (release, `opt-level='s'`, fat LTO)

Measured with `cargo size -p firmware --target thumbv7em-none-eabihf --release`:

| Build | text+data | Headroom in 128 KB |
|---|---|---|
| `--no-default-features` (production, no bell) | ~79 KB | ~49 KB |
| `--no-default-features --features bell` | ~84 KB | ~44 KB |
| `--no-default-features --features "freeze bell"` | ~84 KB | ~44 KB |
| default (`debug-uart`, no bell) | ~129 KB | **~0 — at the ceiling** |
| `--features bell` (`debug-uart` + bell) | **overflows by ~3.6 KB** | — |

**Takeaway:** the *production* image has plenty of room; the **`debug-uart` build
is the one pinned against the 128 KB wall**, so `debug-uart + bell` (or any
future synth voice + the UART log) won't fit on internal flash.

### Why `debug-uart` is so large (~38 KB over the bare build)

- On-device `core::fmt` for all the plaintext diagnostics (the `dbg_uart!`
  machinery, the boot log, the heartbeat, the temp task).
- ~10 KB of **float formatting** (`flt2dec` grisu/dragon + f64 soft-float),
  reachable only in the `debug-uart` build.

**The float formatting is NOT the panic handler** (this was tested — see below).
We removed the *explicit* float formatting from the POS protocol (`usb_cdc.rs`
`{:.3}` → hand-rolled integer `whole.frac`), which made the **production**
(`--no-default-features`) image completely `flt2dec`-free — confirmed with
`cargo nm`, even with `bell` on. So the shipping firmware no longer carries it.

The `debug-uart` build still pulls ~10 KB of float formatting via a *separate*
`core::fmt` path: a `Debug`/`{:?}` of an `f32`-containing value reachable only in
the debug-only code (or a dependency it makes live), referenced through a rodata
vtable that `--gc-sections` keeps. Pinning the exact site down statically is
obscured by `debug = 2` debuginfo, and it wasn't worth chasing further given the
production image is already clean and the real ceiling fix is QSPI.

**Tested dead end — the panic handler.** The panic handler formats `{}` of
`PanicInfo` (`debug.rs`), so the natural guess is that it anchors the float code
(panic/assert messages in deps can format floats). It does not: rewriting it to
print location-only + literal-message-only (no generic message formatting) left
`flt2dec` present and the binary the same size. So reformatting the panic handler
cannot reclaim the ~10 KB — it was reverted to the richer full-message version.

## The migration

Run code from QSPI instead of internal flash. This needs three things:

1. **Daisy bootloader.** Electro-Smith's bootloader lives in the 128 KB internal
   flash, sets up the QSPI peripheral in memory-mapped (XIP) mode at boot, then
   jumps to the application at `0x90000000`. Flash it once with `dfu-util` (or
   `probe-rs`).
2. **QSPI linker layout.** Point `.text`/`.rodata` at the `QSPIFLASH` region
   (already declared in `crates/firmware/memory.x`) instead of `FLASH`. Keep
   vector table / boot shim wherever the bootloader expects.
3. **DFU flashing flow.** The app is loaded into QSPI through the bootloader
   (`dfu-util -a 0 -s 0x90000000:leave -D firmware.bin`) rather than the current
   `probe-rs run --chip STM32H750IBKx` (which writes internal flash directly and
   bypasses any bootloader). Update the `.cargo/config.toml` aliases / runner.

### Open questions to resolve before doing it

- **Does `daisy-embassy` support a QSPI/bootloader build?** Check the pinned rev
  (see `crates/firmware/Cargo.toml`) for a QSPI memory.x / bootloader example or
  feature. libDaisy ships `STM32H750IB_qspi.lds`; we need the embassy equivalent.
- **XIP performance.** Code executes-in-place from QSPI, which is slower than
  internal flash. The H7 instruction cache hides most of it, but pin any
  hot real-time audio functions to RAM (`#[link_section = ".itcm"]` / ramfunc)
  if a callback path gets tight. Measure `CB_FULL_US` after migrating.
- **SDRAM note.** The heap stays in AXI-SRAM by design (see
  `memory.x` and the `daisy-fx-buffers-sdram` rationale) — QSPI is about *code*
  size, not the DSP working-set RAM. The two are independent.

## Interim state (no migration needed yet)

- The bell ships fine in the **production** firmware (`--no-default-features
  --features bell`).
- **`opt-level='z'` workaround (in place — REMOVE after QSPI).** Only the
  *debug* firmware aliases (`firmware`/`flash`/`bin` in `.cargo/config.toml`,
  default features → `debug-uart`) override `opt-level` to `'z'` via `--config`.
  The shared `[profile.release]` stays `'s'`, so **production
  (`flash-prod`/`bin-prod`, `--no-default-features`) is unaffected** and keeps
  `'s'`. The `'z'` override frees ~7.5 KB on the debug build, keeping
  `debug-uart + bell` under the 128 KB ceiling (~127 KB vs an overflow at `'s'`).
  **Revert it (drop the `--config` from the debug aliases) once QSPI lifts the
  flash limit.** Caveat: `'z'` can slow hot DSP loops — verify `CB_FULL_US` /
  `SAI_ERR` stay healthy on hardware; if it regresses audio, narrow it with a
  per-package override that keeps `dsp` faster.

### Flash budget (current)

| Build | opt-level | text+data | vs 128 KB |
|---|---|---|---|
| `flash-prod`/`bin-prod` (production, `--no-default-features`, +bell) | `s` | ~84 KB | ~44 KB free |
| `firmware`/`flash` (debug, `debug-uart`, no bell) | `z` | ~122 KB | ~9 KB free |
| `firmware --features bell` (debug, `debug-uart` + bell) | `z` | ~127 KB | ~4 KB free |
