# Multi-channel I/O — 4× stereo in / 4× stereo out (AK5558 + AK4458)

> **Status: potential future addition. NOT part of the current installation.**
>
> The shipping exhibit is single-stereo: the onboard PCM3060 stereo line-out
> to a PA, stereo USB UAC to the Pi visualizer (see `BREAKOUT.md`,
> `DAISY_I2S_SETUP.md`). This document sketches a *later* installation built
> on the **same Daisy Seed / STM32H750 architecture** that wants four
> independent stereo inputs and four independent stereo outputs — e.g. per-
> source processing of the [4 distant songs](../EXHIBIT.md) or multi-speaker
> spatialization. It is a **paper design**: no board exists, no firmware is
> written, and the pin map below must be verified against the Daisy pinout
> PDF before anyone cuts copper. Treat every number as a starting hypothesis,
> not a validated spec.

---

## 1. Why these two chips

The Daisy's onboard **PCM3060 is a stereo codec** — 2-in / 2-out over a single
I²S link. There is no way to get 8 input + 8 output channels out of it. The
clean way to widen the I/O on the *same* STM32H750 is **TDM** (time-division
multiplexing): one SAI data line carries up to 16 slots per frame, so 8
channels (4 stereo) ride on a single wire instead of four separate I²S links.

| Chip | Role | Channels | Why |
|---|---|---|---|
| **AKM AK5558** | ADC | 8 (4 stereo) in | Single-chip 8-ch ADC, 32-bit, TDM out on one data line, ~112 dB S/N. The de-facto part for DIY 8-in interfaces. |
| **AKM AK4458** | DAC | 8 (4 stereo) out | Single-chip 8-ch DAC, 32-bit, TDM in on one data line, ~115 dB. Velvet Sound family, pairs cleanly with the AK5558 on a shared clock bus. |

They are a matched pair: same vendor, same TDM framing conventions, designed
to hang off a **common master clock bus** (shared MCLK / BCLK / FS). The STM32
is the clock master; both AKM parts are slaves. That keeps every converter on
the board sample-locked with zero resampling.

> **Supply caveat (relevant for a "later" build).** AKM's Nov-2020 fab fire
> caused multi-year shortages and price spikes on exactly this family. Before
> committing, check availability — and keep drop-in-ish alternatives in mind:
> ESS ES9038PRO / ES9016 or TI PCM1690 for the DAC side; two cascaded TI
> TLV320ADC5140 (4-ch each, TDM-daisy-chained) for the ADC side. The
> architecture below is converter-agnostic; only the register init and exact
> rail voltages change.

---

## 2. Architecture overview

```
   4× stereo line-in                                 4× stereo line-out
   (8 jacks)                                          (8 jacks)
        │                                                  ▲
        ▼  analog buffers / anti-alias                     │ analog buffers / recon
   ┌─────────────┐                                  ┌─────────────┐
   │  AK5558      │  8-ch ADC                        │  AK4458      │  8-ch DAC
   │  (TDM out)   │                                  │  (TDM in)    │
   └──────┬───────┘                                  └──────▲──────┘
          │ SD (1 data line, 8 slots)                       │ SD (1 data line, 8 slots)
          │                                                 │
   ┌──────┴─────────────────── shared clock bus ───────────┴──────┐
   │   MCLK (256·fs)   BCLK (fs·8·32)   FS/LRCLK (fs)              │
   └──────────────────────────┬───────────────────────────────────┘
                              │  (+ I²C control bus: SDA/SCL, shared)
                    ┌─────────┴──────────┐
                    │  Daisy Seed Rev 7  │  STM32H750
                    │  SAI1_A = TX master│  → DAC
                    │  SAI1_B = RX sync  │  ← ADC
                    │  I2Cx  = control   │
                    └────────────────────┘
                    │            │
              USB UAC (viz)   onboard PCM3060
                              (unused, or reuse as a 5th out)
```

The onboard PCM3060 is **bypassed** in this design — the Daisy's SAI is
re-tasked to the external TDM bus. (It could optionally remain wired as a
stereo monitor/5th output, but that complicates clocking; the simple path is
to leave it idle.)

---

## 3. Clock & data topology

Full-duplex multichannel on the STM32H750 uses **one SAI block, both
sub-blocks**, as a synchronized master/slave pair — the standard way to run a
codec full-duplex:

| Sub-block | Direction | Clocking | Wired to |
|---|---|---|---|
| **SAI1_A** | Transmit | **Async master** — generates BCLK + FS, emits MCLK | AK4458 data-in (`SDATA`) |
| **SAI1_B** | Receive | **Synchronous slave** — borrows SAI1_A's BCLK/FS | AK5558 data-out (`SDOUT`) |

Both sub-blocks share one bit clock and one frame sync, so ADC and DAC are
sample-aligned by construction. The single MCLK fans out to both AKM parts.

**TDM frame (target: 48 kHz, 8 slots × 32-bit):**

```
BCLK = fs × slots × slot_bits = 48 000 × 8 × 32 = 12.288 MHz
MCLK = 256 × fs               = 12.288 MHz   (256·fs; AK5558/AK4458 accept 256/512fs)
FS   = fs                     = 48 kHz, one pulse per 8-slot frame
```

12.288 MHz BCLK is well inside SAI limits and inside both converters' TDM512
capability. One DMA frame is **8 interleaved samples** `[ch0…ch7]`; firmware
de-interleaves into 4 stereo pairs (and re-interleaves on the way out) — a
trivial copy (see §6).

> Note the framing convention must match on both ends: AKM TDM is
> left-justified with the frame-sync edge marking slot 0. The SAI
> `FrameSyncDefinition` / slot-offset config has to be set to the AKM
> convention, not the I²S-style one-BCLK-delay used for the PCM3060. Getting
> this wrong gives a one-slot channel rotation or a half-frame offset — read
> the AK5558/AK4458 timing diagrams against the SAI slot config carefully.

---

## 4. Pin budget — **must verify against the pinout PDF**

The external bus needs roughly **9 header pins** plus power. The hard
question — same one flagged in `DAISY_I2S_SETUP.md` — is *which* Daisy header
pins can carry SAI alternate functions, given that the Daisy's onboard SDRAM,
QSPI flash, SDMMC, and USB already consume large blocks of the H750's pins,
and the SD-card SPI + MIDI UART from `BREAKOUT.md` claim a few more.

| Signal | Count | Notes |
|---|---|---|
| MCLK | 1 | Master clock to both AKM parts (256·fs). |
| BCLK (SCK) | 1 | Shared bit clock. |
| FS (LRCLK) | 1 | Shared frame sync. |
| SD → DAC | 1 | SAI1_A transmit data to AK4458. |
| SD ← ADC | 1 | SAI1_B receive data from AK5558. |
| I²C SDA | 1 | Shared control bus (two device addresses). |
| I²C SCL | 1 | " |
| ADC reset/PDN | 1 | Can share one GPIO with the DAC if reset timing allows. |
| DAC reset/PDN | 1 | " |

**Open question — the binding constraint.** Are five SAI signals (MCLK, BCLK,
FS, 2× data) routable to *free* header pins on this Daisy? Two sub-approaches,
both from `DAISY_I2S_SETUP.md §Wiring`:

- **(A)** Use a second SAI peripheral whose alternate-function pins land on
  free header pads. Cleanest if the AF map cooperates — needs confirmation
  against the **Electrosmith Daisy Seed pinout PDF** + the STM32H750
  alternate-function table. *Do not guess from memory; the AF tables are
  dense and easy to misread.*
- **(B)** Tap the existing codec-bound SAI1 lines at module test points and
  abandon the PCM3060 entirely. No spare-pin hunt, but requires fine SMD
  soldering on the Seed.

Resolving (A) vs (B) is the **first task** before this design is real. Until a
concrete, verified pad list exists (like the table in `BREAKOUT.md §2`), this
section is a hypothesis.

---

## 5. Analog front/back end

This is a bigger analog board than the current breakout — the AKM parts give
you digital channels, but 8 clean line-level inputs and 8 line-level outputs
need real analog support:

- **Inputs (AK5558):** 8× single-ended or differential line inputs with
  anti-alias RC, input biasing to the ADC's common-mode, and ideally a
  unity/low-gain op-amp buffer per channel. 4 stereo TRS (or TS-pair) jacks.
- **Outputs (AK4458):** voltage-output DAC → per-channel reconstruction
  low-pass + AC-coupling + line-driver op-amp. 4 stereo output jacks.
- **Supplies:** the AKM parts want multiple rails (≈1.8 V core, 3.3 V digital
  I/O, and a clean analog AVDD — 3.3 V on the ADC, up to ~5 V on the DAC for
  output swing). **Low-noise LDOs and a star-grounded analog section are not
  optional here** — converter S/N at the ~112–115 dB spec is only reachable
  with a clean analog supply and careful AGND/DGND separation. This is the
  part most likely to underperform the datasheet if rushed.
- **Clocking quality:** SAI MCLK derived from the H750 PLL is functional, but
  for best converter jitter a dedicated low-jitter audio oscillator
  (12.288 MHz) or a clock chip (e.g. Si5351) feeding MCLK improves THD+N.
  Optional; note it and measure before adding.

This is no longer a hand-soldered perfboard like Board A — realistically it's
a **fabbed PCB** (4-layer, with a poured analog ground) to hit the noise
floor. Plan accordingly for the "later installation" timeframe.

---

## 6. Firmware work

The current firmware (`crates/firmware/src/main.rs`) brings up the PCM3060
stereo path via `daisy-embassy`'s `AudioPeripherals` → `prepare_interface` →
`start_interface` → `start_callback`, with `HALF_DMA_BUFFER_LENGTH`-sized
stereo DMA buffers and an f32→24-bit conversion in the callback. Going
multichannel means **extending or forking that audio setup** — `daisy-embassy`
configures SAI1 for a stereo PCM3060, not an 8-slot TDM bus. Concretely:

1. **SAI TDM config.** Configure SAI1_A (TX master) + SAI1_B (RX sync) for
   8 slots × 32-bit, slot offset per AKM framing. `embassy-stm32`'s SAI driver
   exposes `slot_count` / `slot_size` / `FrameSyncDefinition` — this is hand-
   rolled SAI setup, not the BSP helper.
2. **DMA buffers.** Frame size grows 4× (8 slots vs 2). `HALF_DMA_BUFFER_LENGTH`
   and the SRAM1 `.sram1_bss` non-cacheable region (set up via MPU in
   `main.rs`) must be resized for the wider frame and kept SAI-DMA-coherent.
3. **De-interleave / re-interleave.** The RX DMA buffer arrives as
   `[ch0…ch7]` per frame → split into 4 stereo `&mut [f32]` pairs; run the DSP;
   re-interleave 4 pairs back into the TX buffer. Trivial copies, negligible
   cost.
4. **I²C init sequence.** Bring-up code to reset + register-configure both AKM
   parts (TDM mode, slot mapping, sample rate, de-emphasis off, etc.) over a
   shared I²C bus at boot, before starting the SAI callback. The AKM register
   maps are the fiddly part — budget time for the init/reset ordering.
5. **USB UAC tee.** The visualizer still wants *a* stereo feed; pick one stereo
   pair (or a downmix) to tee into the existing UAC source path
   (`uac_source.rs`), unchanged otherwise.

Real-time discipline from memory still applies: no alloc in the callback, heap
in AXI SRAM, FX buffers in SDRAM, D-cache + MPU honored.

---

## 7. DSP feasibility — already budgeted

The companion question — *"is 4× stereo of low-pass + freeze (tape off)
feasible on the H750?"* — was worked out against the actual DSP source. Summary:

- **CPU: ~25 % of one core**, worst case (all four freezes active). Per stereo
  channel ≈ 600 cycles/frame (SVF low-pass + freeze producer + the freeze's
  `GlitchTape` wow/flutter+chew); ×4 ≈ 2 400 of the 10 000 cycles/frame
  budget at 480 MHz / 48 kHz. No per-sample transcendentals — wow/flutter
  already uses `fast_cos`, not `libm::cosf`.
- **The binding constraint is RAM, not CPU.** Four freeze rings (~115 KB
  each) + four wow/flutter delay lines (~19 KB each) ≈ **0.5 MB**. That
  exceeds AXI SRAM and **must live in SDRAM** (64 MB available), with
  cache-friendly sequential access — which the ring buffers already have.
- **Tape stays off per channel.** A single tape chain (≈140-tap loss FIR +
  RK2 hysteresis) costs more than all eight freeze/low-pass paths combined;
  if tape ever returns on a channel, budget it separately.

So the converters are the work; the DSP fits comfortably. (CPU figures are
paper budgets — confirm on hardware with DWT->CYCCNT once flashed, same
caveat as `TAPE_SIMULATION.md §16`.)

---

## 8. Open questions / risks (in priority order)

1. **Pin routability (§4).** The whole design is gated on whether 5 SAI
   signals + I²C reach free header pads. Resolve against the pinout PDF first.
2. **AKM framing match (§3).** Slot offset / FS convention mismatch is the
   most likely "it compiles but channels are rotated/garbled" bug.
3. **Analog noise floor (§5).** Hitting ~112 dB needs a fabbed PCB with clean
   supplies; a perfboard build will underperform and may not be worth doing.
4. **AKM supply (§1).** Confirm parts are buyable, or commit to an alternative
   converter family early — it changes the register init, not the topology.
5. **`daisy-embassy` fork scope (§6).** TDM isn't in the BSP path; estimate
   the SAI/DMA rework before committing to a build date.

---

## 9. Scope boundary — what this is NOT

- Not needed for, and not part of, the current single-stereo exhibit.
- Not a perfboard mod to Board A (`BREAKOUT.md`) — it's a separate fabbed
  board.
- Not validated: no pin map, no schematic, no firmware, no measurements.
  Promote this to a real plan (à la `PLAN_*.md`) only after §8.1 and §8.2 are
  resolved.

---

## 10. References

- `BREAKOUT.md` — current Board A (stereo line-out, MIDI in, SD); pad-summary
  style this doc should match once pins are verified.
- `DAISY_I2S_SETUP.md` — second-SAI-to-header discussion (approaches A/B) and
  the "don't guess SAI alternate functions from memory" rule.
- `TAPE_SIMULATION.md` — DSP cost reference and the paper-budget caveat.
- `EXHIBIT.md` — the 18-min, 4-distant-song composition this multichannel
  build would serve.
- AKM **AK5558** datasheet — 8-ch ADC, TDM modes, register map, supply rails.
- AKM **AK4458** datasheet — 8-ch DAC, TDM in, register map, AVDD for output
  swing.
- [Daisy Seed pinout PDF](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/Daisy_Seed_datasheet.pdf)
  + STM32H750 reference manual SAI/alternate-function tables — the source of
  truth for §4.
