# Daisy Seed breakout — final design

Hand-soldered protoboard that adds the three things the bare Seed lacks for
this project: a stereo line-out jack, a MIDI input, and an SD card socket.
Audio leaves the system over USB UAC to the Raspberry Pi (visualizer host)
and over the codec's stereo line out to a PA.

---

## 1. System architecture

```
   ┌───────────────────────────────┐
   │  Wall PSU (Pi official        │
   │  5V/3A USB-C supply)          │
   └───────────────┬───────────────┘
                   │  5V
                   ▼
   ┌───────────────────────────────────────────────────────────┐
   │             Raspberry Pi 4 Model B                        │
   │                                                           │
   │   USB-C (power in)                                        │
   │                                                           │
   │   USB-A (host) ◄──── USB UAC + bus power ──────┐          │
   │                                                │          │
   │   GPIO header:                                 │          │
   │     pin  2 (+5V) ─── F-F jumper ──────────┐    │          │
   │     pin  6 (GND) ─── F-F jumper ─────┐    │    │          │
   └──────────────────────────────────────┘    │    │          │
                                          │    │    │
                                  GND     │    │    │ USB cable
                                          │    │    │ (USB-A → Micro USB,
                                          │    │    │  data + power)
                                          │    │    │
   ┌──────────────────────────────────────▼────▼────▼──────────┐
   │            Custom breakout board (perfboard)              │
   │                                                           │
   │  +5V rail ── (6N138 Vcc only)                             │
   │  GND rail ── (common across Daisy GND + Pi GND + 6N138)   │
   │  +3V3 rail ── (from Daisy pad 38, feeds SD + opto pull-up)│
   │                                                           │
   │  ┌───────── Daisy Seed Rev 7 (socketed) ─────────┐        │
   │  │ pad 18 ──► audio TRS TIP                       │        │
   │  │ pad 19 ──► audio TRS RING                      │        │
   │  │ pad 40 ──► audio TRS SLEEVE                    │        │
   │  │ pad 14 ◄── MIDI input from 6N138 output         │       │
   │  │ pad 38 ──► +3V3 rail                            │       │
   │  │ pad 40 ──► GND rail                             │       │
   │  │ pad D7  ──► SD CS                               │       │
   │  │ pad D8  ──► SD SCK                              │       │
   │  │ pad D9  ◄── SD MISO                              │      │
   │  │ pad D10 ──► SD MOSI                              │      │
   │  └─────────────────────────────────────────────────┘       │
   │                                                            │
   │  ┌─ 6N138 block ──┐    ┌─ SD module ─┐    ┌── Jacks ──┐    │
   │  │ (MIDI input)   │    │ (WWZMDiB)   │    │ Audio TRS │    │
   │  │                │    │             │    │ MIDI TRS  │    │
   │  └────────────────┘    └─────────────┘    └───────────┘    │
   └───────────────────────────────────────────────────────────┘

   ── Audio TRS jack ────► to PA / line input
   ── MIDI TRS jack  ◄──── from MIDI controller (Type A)
   ── Daisy USB ─────────► UAC stereo to Pi visualizer
```

### Power rails on the breakout

| Rail | Source | Loads | Notes |
|---|---|---|---|
| **+5V** | Pi GPIO pin 2 or 4 (jumper wire) | 6N138 pin 8 (Vcc only) | ~5 mA draw; trivial vs Pi 5V budget |
| **+3V3** | Daisy pad 38 (3V3D) | SD module Vcc, 6N138 output pull-up | <50 mA total |
| **GND** | Daisy pad 40 + Pi GPIO pin 6 (jumper) | Everything | Single common ground; Pi-Daisy GND already tied via USB shield |

Pi GPIO 5V and USB-C input share the same rail downstream of the Pi's input
protection, so the Daisy's bus-power and the breakout's 5V are siblings of
the same PSU. No ground loop risk because they all reference the same point.

---

## 2. Complete schematic

```
                                                  +5V rail (from Pi GPIO pin 2)
                                                   │
                                                   │
                                                   ┣─[100nF]─ GND
                                                   │
                                                   │                            +3V3 rail (Daisy pad 38)
                                                   │                             │
                                                   │                             │
                                              ┌────┴─────┐                       │
3.5mm TRS (MIDI in,                           │  6N138   │                       │
 Type A wiring):                              │ (DIP-8)  │                      [2.2kΩ]
                                              │          │                       │
   TIP ──[220Ω]──┬───────────────► pin 2 ──── │  Anode   │                       │
                 │                            │          │                       │
                 │  ┌──────┐                  │          │                       │
                 ├──┤1N4148├──┐               │          │                       │
                 │  │  (1) │  │               │          │                       │
                 │  └──────┘  │               │          │                       │
                 │            │               │          │                       │
   RING ─────────┴────────────┴─► pin 3 ──── │ Cathode  │                       │
                                              │          │                       │
   SLEEVE ─ shield only                       │  NC      │ pin 1                 │
   (NOT to GND — preserves isolation)         │  NC      │ pin 4                 │
                                              │          │                       │
                                              │  GND     │ pin 5 ─── GND rail    │
                                              │          │                       │
                                              │  Vo      │ pin 6 ──┬─────────────┘
                                              │          │         │
                                              │  Vb      │ pin 7 ──┤ LEFT OPEN
                                              │          │         │ (no connection)
                                              │  Vcc     │ pin 8 ──┘
                                              └──────────┘         │
                                                                   ▼
                                                              Daisy pad 14
                                                              (D14 / PB7 /
                                                               USART1_RX,
                                                               5V-tolerant)

   (1) 1N4148 orientation: anode toward 6N138 pin 3, cathode toward pin 2.
       Conducts only when MIDI loop is reverse-polarized (fault).


3.5mm TRS (audio out, single stereo jack):

   Daisy pad 18 (AUDIO OUT L) ──────────────────► TIP
   Daisy pad 19 (AUDIO OUT R) ──────────────────► RING
   Daisy pad 40 (DGND)        ──────────────────► SLEEVE

   No external components needed. On-module path per Rev 7 schematic:
   PCM3060 OUT ──[4.7µF]──┬──[100Ω]── pad
                          └──[47kΩ]── AGND


WWZMDiB microSD module (1×6 header) on Daisy SPI1:

   Module pin     Daisy pad    STM32 pin   Function
   ──────────     ─────────    ─────────   ──────────────────
   VCC            pad 38       —           +3V3
   GND            pad 40       —           GND
   MISO           D9           PB4         SPI1_MISO
   MOSI           D10          PB5         SPI1_MOSI
   SCK            D8           PG11        SPI1_SCK
   CS             D7           PG10        SPI1_NSS (sw CS)
```

### Daisy Seed pad summary (only pads we use)

| Pad | Daisy name | STM32 | Role |
|---|---|---|---|
| 14 | D14 | PB7 | MIDI UART RX (USART1_RX) — 5V-tolerant |
| 18 | — | — | AUDIO OUT L → TRS TIP |
| 19 | — | — | AUDIO OUT R → TRS RING |
| 38 | — | — | +3V3D rail source |
| 40 | — | — | DGND |
| 8 | D7 | PG10 | SD CS |
| 9 | D8 | PG11 | SD SCK |
| 10 | D9 | PB4 | SD MISO |
| 11 | D10 | PB5 | SD MOSI |

All other Seed pads remain free.

---

## 3. Confirmed hardware

- **Daisy Seed Rev 7** (silkscreen-verified). Codec is **PCM3060**.
  Repo `README.md` claims "Rev 6, AK4556" — both wrong; fix pending.
- Firmware feature flag: `daisy-embassy = { ..., features = ["seed_1_2"] }`.
  Current `["seed"]` is the Rev 4 / AK4556 profile and will misconfigure
  the SAI clocks for the PCM3060.

---

## 4. Subsystem detail

### 4.1 Stereo line out

Direct pad-to-jack wiring; no external components. The Rev 7 schematic shows
the codec output stage is fully treated on the Seed module:

```
PCM3060 ──[ 4.7µF ]──┬──[ 100Ω ]── pad 18 / 19
                     └──[ 47kΩ ]── AGND
```

Output is line-level (1Vrms @ 0dBFS, 100Ω output impedance).

**Headphone caveat:** the 4.7µF coupling cap rolls off bass into low-impedance
loads (~1 kHz HPF into 32Ω cans). Suitable for line-in destinations only. If
headphone drive is ever needed, that's a separate amp IC (TPA6132 / PAM8908),
not a wiring change.

### 4.2 MIDI input

Classic MIDI reference-design opto (the 6N138 replaced the long-obsolete
PC-900). H11L1 would be the ideal 3.3V-native choice but isn't available
on Amazon next-day; 6N138 is everywhere.

**Critical: 6N138 must be powered from 5V, not 3.3V.** Per Vishay datasheet
#83605, 6N138 absolute-max Vcc = 7 V and all electrical characteristics are
tested at Vcc = 4.5 V; the operating window is effectively 4.5–7 V. We tap 5V
from the Pi 4's GPIO header (pin 2 or 4) since the Daisy doesn't expose 5V.

The open-collector output pull-up still goes to **3.3V**, so the signal swing
into the Daisy UART RX stays at safe 0–3.3V — only the chip's internal bias
gets the 5V it needs.

Galvanically isolated; the TRS sleeve does **NOT** bond to Daisy GND.

**MIDI loop current check.** With Vf ≈ 1.4 V (datasheet typ at IF = 1.6 mA),
loop current through the 220 Ω (TX side, ×2) + 220 Ω (RX side) =
(5 − 1.4) / 660 = **5.45 mA**. Matches the MIDI spec target of 5 mA.

#### Pull-up = 2.2 kΩ — datasheet recommendation

Vishay #83605 explicitly specifies 2.2 kΩ for the 6N138's open-collector
Darlington output. Lower values waste current at the LOW state; higher
values slow the LOW→HIGH transition. **Don't substitute.**

#### Pin 7 (Vb) — leave open

Datasheet note: *"Using a resistor between pin 5 and 7 will decrease gain
and delay time."* For MIDI we want maximum sensitivity at the 5 mA loop
current, so pin 7 stays unconnected. Do **NOT** add a pull-down resistor —
this is a common Internet copy-paste error.

#### Propagation delay caveat

Datasheet 6N138 tpHL_max = 10 µs (LED on, output → LOW — START bit edge)
and tpLH_max = 35 µs (LED off, output → HIGH — STOP bit edge). MIDI bit
time = 32 µs, so the worst-case-spec'd tpLH is *just* over a bit time.
Typical units measure 2–5 µs in practice; that's where decades of MIDI
6N138 designs draw their reliability from. H11L1's 4 µs max would have
been more comfortable, but 6N138 typical is fine.

### 4.3 SD card

The purchased WWZMDiB modules expose only 6 pins, so 4-bit SDMMC is not
available without hacking the card slot. Use SPI mode instead — plenty fast
for sample streaming (12–25 MHz SPI ≈ 1–3 MB/s sustained, comfortable for
an ambient sampler).

**Firmware change required:** swap `SdmmcHandler` for an SPI-driven FAT
stack (`embedded-sdmmc` over `embassy-stm32` SPI). Not yet implemented.

### 4.4 Debug — STLINK or DFU

The STLINK-V3MINIE has an STDC14 connector (14-pin, 1.27 mm); the Daisy's
P6 is MIPI-10 (10-pin, 1.27 mm). Per ST UM2910 the probe ships with only
an STDC14-to-STDC14 ribbon cable. STDC14 is NOT directly compatible with
MIPI-10 (although pins 3–12 carry the same signals).

Options:

1. **USB DFU — recommended first.** Use the `cargo bin` + `dfu-util` workflow
   in `README.md`. Hold BOOT, tap RESET, release BOOT. No extra hardware.
   Sufficient for the entire bring-up.
2. **STDC14-to-MIPI10 adapter cable** (ST sells one; third parties too)
   if you want SWD + `defmt-rtt` live logging via `cargo flash`.
3. **Shelf the STLINK.** It was bought before this was understood. Bring it
   out when SWD/RTT becomes necessary.

### 4.5 MIDI activity indicator

Blink the Seed's onboard user LED (PC7 / Daisy D31) on UART RX byte. No
external indicator on the breakout. Firmware implementation pending.

---

## 5. Bill of Materials

### To buy

| Qty | Part | Notes / Amazon |
|---|---|---|
| 1 | 6N138 optocoupler, DIP-8 | Vishay or equivalent |
| 2 | 3.5mm TRS panel-mount jack | Audio out + MIDI in |
| 1 | 1N4148 diode (DO-35) | MIDI reverse-polarity protection |
| 1 | 220Ω 1/4W 5% carbon film | MIDI LED current limit |
| 1 | 2.2kΩ 1/4W 5% carbon film | 6N138 output pull-up |
| 1 | 100nF ceramic cap, ≥6.3V | 6N138 Vcc bypass |
| 1 | Perfboard ~90×70mm (3.54"×2.75") | Individual-pad-per-hole, NOT stripboard |
| 1 pack | 1×20 single-row 2.54mm female header strips | Socket for Seed; use 2 of pack. Amazon B0CFDV41T9 confirmed compatible |
| 1 pack | 1×6 single-row 2.54mm female header strips | Socket for SD module. Amazon B00GYRNAMS confirmed compatible |
| 2 | Female-to-female Dupont jumper wires (~20 cm) | Pi GPIO pin 2 (+5V) and pin 6 (GND) → breakout |
| 1 (opt.) | STDC14-to-MIPI10 adapter | Only if SWD debugging desired later |

**Do NOT buy** the IWISS B08X6C7PZM Dupont *crimp connector kit* for the
Seed sockets — that's for assembling custom jumper wires, not for
board-mounted socketing.

### On-hand

- Daisy Seed Rev 7 (PCM3060 codec)
- WWZMDiB microSD SPI modules ×6 (Amazon B0BV8ZQ81F)
- STLINK-V3MINIE programmer (Amazon B0BGJ8RD4N) — needs MIPI-10 adapter
- USB-A → Micro USB data cable
- Pi 4 Model B + official 5V/3A USB-C PSU
- 1/4W 5% carbon film resistor kit (contains all needed values)
- BOJACK-grade cheap ceramic cap assortment

---

## 6. Component grade rationale

| Component | Spec | Why this is fine |
|---|---|---|
| Resistors | 1/4W 5% carbon film | Worst-case 5 mW dissipation (45× headroom); zero precision-sensitive role |
| Decoupling cap | Cheap ceramic, any dielectric, ≥6.3V | Shunts MHz noise; dielectric quality irrelevant for decoupling |
| 1N4148 | DO-35 | 100 V Vrrm, 300 mA continuous If, 2 A surge vs. ~5 mA fault current |
| 6N138 | DIP-8 | MIDI 1.0 reference opto; 25 mA If max vs 5.45 mA used; 7 V Vcc max vs 5 V used |
| Audio jack | 1× stereo 3.5mm TRS | Single cable to PA via TRS→2×TS adapter; saves panel space |
| MIDI jack | 1× 3.5mm TRS, Type A wiring | Modern MIDI standard |

---

## 7. Firmware work

### Already applied (uncommitted in working tree)

1. **`daisy/crates/firmware/Cargo.toml`** — `daisy-embassy` feature is now
   `"seed_1_2"` (was `"seed"`, the Rev 4 / AK4556 profile).
2. **`daisy/README.md`** — hardware target now reads
   "Rev 7, PCM3060 codec, ... SD card adapter wired to SPI"
   (was the incorrect "Rev 6, AK4556" + "SDMMC1").

These are uncommitted edits in the working tree as of writing this section.

### SD card construction path — compile-checked (3.)

`crates/firmware/src/sd.rs` builds the full SD card stack — `embedded-sdmmc`
v0.9 + `embedded-hal-bus` v0.3 `ExclusiveDevice` wrapping an
`embassy_stm32::spi::Spi<'a, Blocking, Master>` on SPI1 with a GPIO CS.
`main.rs` calls `sd::build_sd_card(p.SPI1, board.pins.d8, d10, d9, d7)`
during boot, proving:

- Crate versions are mutually compatible (no resolver conflicts).
- The `Peri<'_, T>` wrapping that embassy-stm32 0.6 uses lines up with what
  `daisy_embassy::pins::SeedPinN` exposes after `new_daisy_board!`.
- `p.SPI1` and `p.USART1` survive the partial move from `new_daisy_board!`
  and remain available for our own peripheral setup (`new_daisy_board!`
  doesn't claim them).
- `SdCard::new(SpiDevice, Delay)` accepts the constructed device chain.

The card is NOT initialised — `num_bytes()` / `VolumeManager` calls would
block forever waiting for hardware that doesn't exist yet. Those go in
during physical bring-up. What we've proven is that the *integration
hypothesis is sound*: the dependency graph compiles, the type chain links,
the pins map.

Resulting artefact: `target/thumbv7em-none-eabihf/release/firmware` (~2 MB
ELF with debuginfo) and `target/firmware.bin` (~35 KB) DFU-flashable.

### Blocked on physical bring-up

Firmware is at roadmap step 1 of 7 (`main.rs` is a 500 ms blinky plus the
unused `_sdcard` binding). The remaining items below require building
features that don't yet exist and cannot be meaningfully tested before the
breakout is built:

4. **UART MIDI input + activity LED (roadmap step 5).** Configure USART1
   RX on PB7 (Daisy pad D14 / `board.pins.d14`), spawn a reader task that
   decodes incoming MIDI bytes → `dsp::Engine::handle_midi`. On each byte
   arrival, toggle the onboard user LED (PC7 / D31) for a brief flash so
   physical MIDI activity is visible without instrumentation.
5. **Actually drive the SD card during boot.** Call `_sdcard.num_bytes()`
   inside a fallible init routine, then construct a `VolumeManager` with
   the `sd::ZeroTime` stub, open `VolumeIdx(0)`, open the root dir, and
   stream sample bytes into a ring buffer per the README's sample-storage
   plan. The construction surface is already in place — just needs the
   actual block-device calls plumbed in once a card is present.

---

## 8. References

- [SparkFun MIDI Tutorial — Hardware & Electronic Implementation](https://learn.sparkfun.com/tutorials/midi-tutorial/hardware--electronic-implementation) — canonical MIDI 1.0 reference circuit; identifies 6N138 as the modern replacement for the obsolete PC-900
- [diyelectromusic — MIDI In for 3.3V Microcontrollers](https://diyelectromusic.com/2021/02/15/midi-in-for-3-3v-microcontrollers/) — explicit comparison of 6N138 vs H11L1 at 3.3V; explains why 6N138 wants 5V
- Vishay **6N138 datasheet #83605** Rev 1.6 — pinout, Vcc 4.5–7 V, 2.2 kΩ pull-up, Vb (pin 7) leave open, tpHL/tpLH max
- Vishay **1N4148 datasheet #81857** Rev 1.6 — Vrrm 100 V, IF cont 300 mA, IFSM 2 A
- [Daisy Seed datasheet v1.2.0](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/Daisy_Seed_datasheet.pdf) — pin functions, audio characteristics, 5V-tolerant GPIO list, P6 SWD header
- [Daisy Seed Rev 7 schematic](https://daisy.nyc3.cdn.digitaloceanspaces.com/products/seed/ES_Daisy_Seed_Rev7.pdf) — confirms PCM3060 output stage (4.7 µF + 100 Ω + 47 kΩ AGND pulldown per channel) is already on-module
- **Raspberry Pi 4 Model B datasheet RP-008341-DS** Rel 1.1, Mar 2024 — confirms 5V GPIO pins are tied directly to the USB-C VIN rail; max GPIO 5V current not formally published by the Foundation
- **ST UM2910** — STLINK-V3MINIE STDC14 pinout; pins 3–12 are MIPI-10 compatible but no MIPI-10 adapter ships in box
