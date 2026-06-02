# Kiosk Sensor Hardware Handoff

Target platform: **Raspberry Pi** (any model with 40-pin GPIO header and I2C). All sensors operate at **3.3V** logic; no level shifting required.

> **Physical build (2026): two soldered perfboards, not a breadboard.**
> Breadboard spring contacts drift intermittent over an exhibit's run; soldered
> joints don't. The system is split into:
> - **Board A — Daisy audio breakout** (`daisy/BREAKOUT.md`): stereo line out,
>   MIDI in, SD card. Taps Pi **5V + GND**.
> - **Board B — kiosk sensor board** (this doc): I²C sensors + TLC555 breath
>   oscillator + AM312. Taps Pi **3V3 + GND + I²C + GPIOs**.
>
> Each board has its own small Pi-entry male header; F-F Dupont jumpers run
> from the Pi GPIO to those headers. **Strain-relieve the jumper bundle**
> (zip-tie/adhesive-mount it within a few cm of the Pi header, and a dab of
> hot glue over the seated Dupont housings) — the female Dupont on the Pi pin
> is the one remaining loosen-prone contact once the boards are soldered.

## Purpose

An interactive kiosk that responds to:

- **Room entry / motion presence** (AM312 PIR)
- **Person's proximity to the kiosk** (VL53L1X ToF) — drives a smooth, continuous visual effect as they approach
- **Breath puff directed at the kiosk** (HR202 humidity sensor via 555 oscillator) — triggers a discrete visual event
- **Capacitive touch input** (MPR121) — touch-based interaction

The ADS1115 is on the bus but no analog sensors are currently attached. It is available for future expansion (e.g., a thermistor for breath-detection fast path, additional analog inputs).

---

## Bill of Materials

| Component | Part | Qty | Notes |
|---|---|---|---|
| Motion sensor | AM312 PIR module | 1 | 3.3V native, Fresnel lens included |
| Distance sensor | VL53L1X breakout (GY-VL53L1X / TOF400C, or Adafruit #3967) | 1 | Time-of-flight, I2C |
| Capacitive touch | MPR121 breakout (Adafruit #1982 or clone) | 1 | I2C, 12 touch channels |
| ADC | ADS1115 breakout | 1 | I2C, 4 single-ended channels (currently unused) |
| Humidity sensor | HR202 | 1 | Resistive, requires AC excitation via 555 |
| Timer IC | TLC555CP (DIP-8) | 1 | CMOS 555, runs at 3.3V |
| Timing capacitor | 4.7nF / 100V / 5% ceramic (C0G) | 1 | For 555 timing network |
| Timing resistor | 10kΩ | 1 | For 555 R1 |
| Decoupling cap | 0.1µF ceramic | 2-3 | Pin 8 of 555, sensor Vcc rails |
| Bypass cap | 10nF ceramic (or 0.1µF acceptable) | 1 | Pin 5 of 555 |
| I2C pull-ups | 4.7kΩ resistor | 2 | One on SDA, one on SCL, to 3.3V |
| Board | Pad-per-hole perfboard | 1 | Sensor board (Board B). Soldered, not breadboard — see Physical Build note |
| Sockets/headers | 2.54mm female header strips + DIP-8 socket | — | Socket the sensor modules + TLC555 |
| Cat5/Cat5e cable | ~2.5 m | 1 | Remote VL53L1X run — see "Remote VL53L1X over Cat5" |
| Misc | F-F Dupont jumpers, hookup wire | — | Pi GPIO → board entry header |

---

## I2C Bus

All I2C devices share the Pi's hardware I2C bus on GPIO2 (SDA) and GPIO3 (SCL).

| Device | Address | Notes |
|---|---|---|
| VL53L1X | 0x29 | Default; reassignable via XSHUT pin if needed |
| ADS1115 | 0x48 | Default (ADDR pin to GND) |
| MPR121 | 0x5A | Default (ADDR pin to GND) |

**Required:** External I2C pull-up resistors, **4.7kΩ** each, from SDA to 3.3V and from SCL to 3.3V. Install once for the whole bus — not per device. The cheap VL53L1X breakouts in particular often have weak/missing pull-ups and benefit substantially from proper external pulls.

**Verification:** `sudo i2cdetect -y 1` should reliably show all three addresses. If devices appear/disappear between scans, the bus has integrity issues — check pull-ups, wire length, and decoupling first.

**Bus speed:** Default 100 kHz is fine. If issues arise, drop to 50 kHz via `dtparam=i2c_arm_baudrate=50000` in `/boot/firmware/config.txt`.

---

## Pin Assignments (Raspberry Pi)

| Pi Pin (BCM) | Physical Pin | Function | Connected To |
|---|---|---|---|
| GPIO2 | 3 | I2C SDA | VL53L1X, ADS1115, MPR121 (shared) |
| GPIO3 | 5 | I2C SCL | VL53L1X, ADS1115, MPR121 (shared) |
| GPIO4 | 7 | Digital input | AM312 OUT |
| GPIO17 | 11 | Digital input (frequency counter) | TLC555 pin 3 (OUTPUT) |
| GPIO27 | 13 | Digital input (interrupt) | MPR121 IRQ |
| 3.3V | 1, 17 | Power | All sensors + TLC555 + I2C pull-ups |
| GND | 6, 9, 14, 20, etc. | Ground | All sensors common ground |

GPIO4, GPIO17, and GPIO27 are arbitrary choices — any free GPIO works. Update the backend config if reassigned. The MPR121 IRQ is optional but strongly preferred over polling.

---

## Per-Sensor Detail

### AM312 PIR Motion Sensor

**Purpose:** Detect that a person has entered the kiosk's vicinity. Used as a coarse presence signal (e.g., wake the display from idle, start sampling other sensors more aggressively).

**Wiring:**

| AM312 Pin | Connect To |
|---|---|
| VCC | Pi 3.3V |
| OUT | Pi GPIO4 |
| GND | Pi GND |

**Behavior:**

- Output goes HIGH (3.3V) when motion is detected, LOW after a hold period (~2 seconds, fixed; not adjustable on AM312).
- **Power-up settling time: 30-60 seconds.** During this window the output is unreliable and will false-trigger. The backend must suppress all events for the first 60 seconds after boot.
- Detection range with included Fresnel lens: ~3-5m, ~100° FOV.
- Blind to motion *directly toward* the sensor; responds best to lateral motion across zones.
- Sees only changes in IR — a person standing perfectly still becomes invisible after a few seconds.

**Software interface (gpiozero):**

```python
from gpiozero import MotionSensor
pir = MotionSensor(4)
pir.when_motion = on_motion_callback
pir.when_no_motion = on_no_motion_callback
```

**Backend behavior notes:**

- Treat as edge-triggered: emit `motion_started` event on rising edge, `motion_ended` on falling edge (after the AM312's internal hold time elapses).
- Do not emit events during the first 60 seconds post-boot.
- Debounce is handled by the AM312 itself; no software debouncing needed.

---

### VL53L1X Time-of-Flight Distance Sensor

**Purpose:** Measure the distance from the kiosk to the closest object in front of it (intended target: a person's torso). Drives a continuous visual effect that intensifies as they approach.

**Wiring:**

| VL53L1X Pin | Connect To |
|---|---|
| VIN / VCC | Pi 3.3V |
| GND | Pi GND |
| SDA | Pi GPIO2 (shared I2C SDA) |
| SCL | Pi GPIO3 (shared I2C SCL) |
| XSHUT, GPIO1 | Leave unconnected |

**Configuration:**

| Parameter | Value | Rationale |
|---|---|---|
| Distance mode | **Short** | Best accuracy and ambient light immunity for <1.5m kiosk range |
| Timing budget | **20 ms** | Yields ~50 Hz update rate |
| Inter-measurement period | 25 ms | Slightly longer than timing budget |
| ROI (region of interest) | **4×4 SPAD** (~15° FOV) | Narrow cone to reject off-axis objects |

**Mounting:** Aim horizontally at adult chest/torso height. Avoid aiming at the floor, kiosk edge, or anywhere a stationary object might fall within the cone.

**Software interface (CircuitPython):**

```python
import board, busio, adafruit_vl53l1x

i2c = busio.I2C(board.SCL, board.SDA)
sensor = adafruit_vl53l1x.VL53L1X(i2c)
sensor.distance_mode = 1   # 1 = short, 2 = long
sensor.timing_budget = 20  # ms
sensor.start_ranging()

while True:
    if sensor.data_ready:
        distance_cm = sensor.distance  # None if no valid target
        sensor.clear_interrupt()
        # process distance_cm
```

**Detection logic for the backend:**

1. **Smooth raw readings** with an exponential moving average: `smoothed = α * new + (1 - α) * smoothed` with `α ≈ 0.25`. Optionally use a one-euro filter for adaptive smoothing if jitter at rest is objectionable.
2. **Handle invalid reads** (`distance` is `None`): treat as "no target / far" and decay the visual effect toward its idle state. Do not freeze the last valid reading indefinitely.
3. **Engagement zones:**
   - `distance >= 100 cm` → idle state (visual at rest)
   - `25 cm < distance < 100 cm` → active state, map distance to visual intensity
   - `distance <= 25 cm` → lock to full intensity (dead zone for close inspection)
4. **Non-linear mapping** from distance to visual parameter feels better than linear:
   `intensity = 1 - ((distance - 25) / 75)²` for the active zone, clamped to [0, 1].
5. **Multiple people:** sensor naturally returns the closest object in its narrow cone, which is the desired behavior. No additional logic needed.

#### Remote VL53L1X over Cat5 (2 m run)

The VL53L1X must sit at the kiosk face (aimed at chest height) while Board B
lives elsewhere — ~2 m away. Run it over one **Cat5/Cat5e** cable on twisted
pairs, not loose hookup wire.

**Why 2 m is fine.** I²C's real limit is bus capacitance (400 pF spec). 2 m of
Cat5 pair ≈ 100 pF, + ~40 pF devices/strays ≈ **~140 pF — comfortably under**.
With the Pi's internal ~1.8 kΩ pull-ups in parallel with the bus 4.7 kΩ
(≈ 1.3 kΩ effective, see Known Gotchas), edge rise time is ~150 ns — well
inside the 1000 ns budget at 100 kHz. So **keep the 4.7 kΩ pull-ups and
100 kHz; no change for 2 m.** Twisting handles the only real risk over the
run: noise pickup. Do **not** drop pull-ups to a lower value as if for a bare
long wire — the Pi internal pulls already make the effective value strong.

**Pair assignment** — each fast signal twisted with its own ground return;
**SDA and SCL in separate pairs** (never the same pair, or SCL crosstalks
into SDA):

| Cat5 pair | Conductor A | Conductor B |
|---|---|---|
| 1 | SDA | GND |
| 2 | SCL | GND |
| 3 | 3V3 | GND |
| 4 | spare (2nd 3V3+GND, or XSHUT+GND if you add sensors later) | |

**Ground = both ends, one net.** Every GND conductor bonds to Board B's GND
bus at the Pi end **and** to the VL53L1X's single GND pin at the sensor end.
They are not separate grounds — it's one ground net spread across three
conductors so each twisted pair has a local return, which is what makes the
twist reject noise. A ground left floating at the sensor end is dead copper
and defeats the pairing.

**Cable shield** (only if you actually have FTP/STP — most Cat5 is unshielded
UTP): ground the foil/drain at the **Pi end only**. That one-end rule applies
to the *shield*, not to the twisted-pair grounds. Don't conflate them.

**At the sensor end:** 0.1 µF **and** 10 µF from 3V3→GND right at the VL53L1X
breakout — the VCSEL current pulses need a local reservoir 2 m of thin wire
can't supply. Tie XSHUT high to local 3V3 (single sensor, address 0x29).

**Validate:** `i2cdetect -y 1` must show 0x29 on every scan. Any flicker →
drop to 50 kHz (`dtparam=i2c_arm_baudrate=50000`) before suspecting anything
else.

**Adding more distance sensors:** the wiring is trivial (tap SDA/SCL/3V3/GND);
the blocker is the **0x29 address collision** — every VL53L1X boots at 0x29.
Solve with XSHUT sequencing (1 GPIO per sensor, bring up + readdress one at a
time) or a TCA9548A I²C mux. Splitting wires does not fix this.

---

### HR202 Humidity Sensor (via TLC555 Oscillator)

**Purpose:** Detect a breath puff directed at the kiosk. The HR202's resistance drops sharply when humid breath hits it; this changes the 555's output frequency, which the Pi counts.

**This is a triggering application, not a measurement application.** The backend does not need absolute humidity — it watches for rapid changes against a rolling baseline.

#### Circuit

Standard 555 astable with the HR202 in the timing network as R2:

```
Vcc 3.3V ─┬─────────────────────────────────────┐
          │                                     │
          ├─── 0.1µF ─── GND  (Vcc bypass)      │
          │                                     │
          │         ┌─── R1 (10kΩ) ────┬── pin 7 (DISCHARGE)
          │         │                   │
          │         │                   ├── pin 6 (THRESHOLD)
          │         │                   │
          │         │                   R2 = HR202
          │         │                   │
          │         │                   ├── pin 2 (TRIGGER)
          │         │                   │
          │         │                   C1 = 4.7nF (C0G)
          │         │                   │
          │         │                   └── GND
          │         │
          ├── pin 8 (VCC)                         pin 3 (OUTPUT) ──► Pi GPIO17
          ├── pin 4 (RESET, tied high)            pin 1 (GND) ──── GND
          │
          pin 5 (CONTROL) ── 10nF ── GND
```

**Component values:**

- R1 = 10kΩ (standard 1/4W resistor)
- C1 = 4.7nF, 100V, 5%, C0G dielectric (timing stability matters here)
- Pin 5 bypass = 10nF, or 0.1µF acceptable
- Vcc decoupling at pin 8 = 0.1µF ceramic
- IC: **TLC555CP** (CMOS — required, not a bipolar NE555)

**Expected frequency range** (theoretical, `f = 1.44 / ((R1 + 2·R2) · C1)`):

| HR202 R | Approx. Humidity | Output Frequency |
|---|---|---|
| 10 kΩ | ~90% RH | ~10.2 kHz |
| 100 kΩ | ~40% RH (room baseline) | ~1.5 kHz |
| 1 MΩ | ~20% RH | ~150 Hz |

A breath puff at room baseline will spike the frequency from ~1.5 kHz toward 5-10 kHz briefly, then decay back to baseline over 2-10 seconds.

#### Software interface (pigpio)

Frequency counting on a GPIO. Requires `pigpiod` running (`sudo systemctl enable --now pigpiod`).

```python
import pigpio
import time

pi = pigpio.pi()
pi.set_mode(17, pigpio.INPUT)

def measure_frequency(gpio, window_s=0.2):
    count = [0]
    def cb_func(g, level, tick):
        count[0] += 1
    cb = pi.callback(gpio, pigpio.RISING_EDGE, cb_func)
    time.sleep(window_s)
    cb.cancel()
    return count[0] / window_s
```

#### Detection logic for the backend

The HR202 is being used as a **breath trigger**, not a humidity meter. The algorithm:

1. **Maintain a rolling baseline** of frequency using a slow exponential moving average (e.g., `baseline = 0.98 * baseline + 0.02 * current` updated every 200 ms). This absorbs slow drift from room humidity, HVAC, weather.
2. **Compute fast frequency** in a 200 ms window.
3. **Detect breath event** when `fast_freq > baseline * 1.3` (30% above baseline). Tune threshold empirically — 1.2-1.5× works depending on placement.
4. **Debounce:** once a breath event fires, suppress further events for ~3 seconds while the sensor recovers. Subsequent puffs during the decay tail are physically real but produce smaller signals.
5. **Pause baseline updates during and after a breath event** so the baseline doesn't drift up to track the breath itself.

**Response time:** Leading edge of a breath puff is detectable within ~200-500 ms (limited by the measurement window). The sensor's recovery is slow (several seconds) and unrelated to how long the visual effect should last — drive the visual effect on a fixed animation timeline, not on the sensor decay curve.

**Placement:** Sensor element should be 5-10 cm from where the user's mouth will be. Beyond ~15 cm the breath disperses and SNR drops sharply. A clear "blow here" affordance in the physical design is necessary; passive detection at typical kiosk viewing distance will not work reliably.

---

### MPR121 Capacitive Touch Controller

**Purpose:** Touch input for direct interaction. Twelve independent electrode channels.

**Wiring:**

| MPR121 Pin | Connect To |
|---|---|
| VIN / VCC | Pi 3.3V |
| GND | Pi GND |
| SDA | Pi GPIO2 (shared) |
| SCL | Pi GPIO3 (shared) |
| IRQ | Pi GPIO27 (recommended) |
| ADDR | GND (sets address to 0x5A) |

Electrodes E0-E11 connect to whatever conductive surfaces the kiosk uses as touch targets.

**Software interface (CircuitPython):**

```python
import board, busio, adafruit_mpr121

i2c = busio.I2C(board.SCL, board.SDA)
mpr = adafruit_mpr121.MPR121(i2c)
# mpr[0].value through mpr[11].value give per-channel touch state
```

**Backend behavior notes:**

- Use the IRQ pin (GPIO27) for interrupt-driven reads instead of polling. Falling edge = state change.
- The MPR121 handles its own debouncing and threshold detection. The backend should react to state-change events, not raw electrode readings.
- Per-channel touch/release thresholds can be tuned via the library if any channel proves over- or under-sensitive after physical installation.

---

### ADS1115 ADC (Reserved / Future Expansion)

**Purpose:** Currently unused. Available on the I2C bus for future analog sensors.

**Wiring:**

| ADS1115 Pin | Connect To |
|---|---|
| VDD | Pi 3.3V |
| GND | Pi GND |
| SDA | Pi GPIO2 (shared) |
| SCL | Pi GPIO3 (shared) |
| ADDR | GND (sets address to 0x48) |
| A0-A3 | Available for analog inputs |

The backend code should initialize the ADS1115 but not require any channels to be active. If a thermistor or other analog sensor is added later (e.g., for breath fast-path detection), it can be wired to A0-A3 and read at ±4.096V gain, single-ended, without changing the core architecture.

---

## Wiring Checklist (perfboard build — Board B)

1. **Power and ground buses.** Run bare tinned-wire **3.3V** and **GND** buses across the board (solder to each pad they cross that needs them). On perfboard a continuous soldered GND bus is low-impedance enough to satisfy the star-ground intent — you don't need individual returns to one point the way a breadboard's resistive jumper chains demand. Feed the buses from the Pi-entry header.
2. **I2C pull-ups.** Two 4.7kΩ resistors: one SDA→3.3V, one SCL→3.3V. Install once, near where the I2C bus enters the board. (Effective value is ~1.3kΩ with the Pi's internal pulls in parallel — fine.)
3. **Decoupling.** 0.1µF ceramic from Vcc to GND at each sensor module (most have one onboard; adding one doesn't hurt). One on the TLC555 pin 8 is mandatory.
4. **TLC555 circuit.** Build per the schematic above (socket the DIP-8). Verify with a multimeter or scope that pin 3 is oscillating before connecting to the Pi GPIO. With the HR202 in normal room air, expect ~1-2 kHz at pin 3.
5. **I²C wire length.** On-board runs: keep short, no special handling. The **remote VL53L1X at 2 m goes over Cat5 twisted pairs** — see "Remote VL53L1X over Cat5" above for the pair assignment and grounding. Don't run 2 m of loose hookup wire.
6. **AM312 placement.** Mount where motion crosses laterally, not toward the sensor. For a kiosk this typically means perpendicular to the approach path.
7. **VL53L1X placement.** Aim at adult chest height, horizontally, with nothing stationary in the 15° cone within 1 m. (Mounted remotely on the Cat5 run.)
8. **HR202 placement.** Exposed, on a short standoff, oriented toward where the user's face will be. Protect from physical contact but keep airflow unrestricted.

### Board B perfboard layout

The I²C sensor *modules* (MPR121, ADS1115) and the AM312 mount on female-header
sockets or short flying leads where the kiosk needs them aimed. The only
hand-built circuit on the board is the TLC555 oscillator, the I²C pull-ups,
the bus distribution, and the connectors.

```
   ↑ top edge — Pi-entry header (F-F jumpers from Pi GPIO) ↑
  ┌──────────────────────────────────────────────────────────┐
  │ [Pi entry 7p: 3V3 GND SDA SCL G4 G17 G27]                 │
  │  3V3 bus ═══════════════════════════════════════════      │
  │  SDA bus ───────────────────────────────────────────      │
  │  SCL bus ───────────────────────────────────────────      │
  │ [4.7k SDA→3V3] [4.7k SCL→3V3]                             │
  │                                                            │
  │  ┌─ TLC555 ─┐  R1 10k   ┌ MPR121   ┐   ┌ ADS1115  ┐       │
  │  │  DIP-8   │  C1 4.7nF  │  socket  │   │  socket  │       │
  │  │  socket  │  (C0G)     │ +IRQ→G27 │   │          │       │
  │  └──────────┘  pin5 10nF └──────────┘   └──────────┘       │
  │   HR202 on leads →   ▤0.1µF                                │
  │  [AM312 3p: VCC OUT→G4 GND]   [Cat5 landing → VL53L1X]     │
  │  GND bus ═══════════════════════════════════════════      │
  └──────────────────────────────────────────────────────────┘
```

**Pi-entry header (7-pin)** — F-F jumpers from these Pi physical pins:

| Board pin | Pi physical pin | Signal |
|---|---|---|
| 1 | 1 | 3V3 → 3V3 bus |
| 2 | 6 | GND → GND bus |
| 3 | 3 | SDA → SDA bus |
| 4 | 5 | SCL → SCL bus |
| 5 | 7 | GPIO4 ← AM312 OUT |
| 6 | 11 | GPIO17 ← TLC555 pin 3 |
| 7 | 13 | GPIO27 ← MPR121 IRQ |

**Buses → loads:** 3V3 bus to every module VCC/VDD/VIN + both pull-up tops +
TLC555 pin 8. GND bus to every module GND + TLC555 pin 1 + pull-down legs +
the Cat5 GND conductors. SDA/SCL buses to MPR121, ADS1115, and the Cat5
landing for the remote VL53L1X. Per-module pinouts are in the per-sensor
tables above — wire by silkscreen label.

**Cat5 landing:** a small header where the 2 m cable terminates — taps SDA,
SCL, 3V3, GND off the buses onto the twisted pairs (pairing per the Cat5
section). All GND conductors to the GND bus.

### Board B — minimal variant (MPR121 + VL53L1X only)

For the **first board to build**, use only the two I²C devices: **MPR121**
(touch) and **VL53L1X** (distance, remote over Cat5). This omits:

- **TLC555 + HR202** (breath trigger) — the entire oscillator circuit, R1, C1,
  the two timing caps, and GPIO17.
- **ADS1115** (unused ADC) — and its bus tap.
- **AM312** (PIR motion) — and GPIO4.

Both remaining devices are I²C, so the board reduces to: bus distribution +
the two pull-ups + one MPR121 socket + the Cat5 landing. It fits on a much
smaller perfboard than the full version.

```
   ↑ top edge — Pi-entry header (5 F-F jumpers from Pi GPIO) ↑
  ┌────────────────────────────────────────────────────────┐
  │ [Pi entry 5p: 3V3  GND  SDA  SCL  G27]                  │
  │  3V3 bus ══════════════════════════════════════         │
  │  SDA bus ──────────────────────────────────────         │
  │  SCL bus ──────────────────────────────────────         │
  │ [4.7k SDA→3V3]  [4.7k SCL→3V3]                          │
  │                                                          │
  │   ┌─ MPR121 socket ─┐         [Cat5 landing → VL53L1X]  │
  │   │ VCC GND SDA SCL  │          SDA  SCL  3V3  GND        │
  │   │ IRQ→G27  ADDR→GND│                                   │
  │   └──────────────────┘                                  │
  │  GND bus ══════════════════════════════════════         │
  └────────────────────────────────────────────────────────┘
```

**Pi-entry header (5-pin)** — F-F jumpers from these Pi physical pins:

| Board pin | Pi physical pin | Signal |
|---|---|---|
| 1 | 1 | 3V3 → 3V3 bus |
| 2 | 6 | GND → GND bus |
| 3 | 3 | SDA → SDA bus |
| 4 | 5 | SCL → SCL bus |
| 5 | 13 | GPIO27 ← MPR121 IRQ |

**Buses → loads:**
- **3V3 bus** → MPR121 VCC, both pull-up tops, Cat5 3V3 conductor.
- **GND bus** → MPR121 GND, MPR121 ADDR (sets address 0x5A), Cat5 GND conductors.
- **SDA bus** → MPR121 SDA, Cat5 SDA.
- **SCL bus** → MPR121 SCL, Cat5 SCL.
- **GPIO27** ← MPR121 IRQ (direct, not a bus).

**IRQ is optional** — if you'd rather poll the MPR121, drop GPIO27 and the
Pi-entry header becomes 4 pins (3V3/GND/SDA/SCL). IRQ is still recommended.

**The Cat5 remote VL53L1X wiring is unchanged** — see "Remote VL53L1X over
Cat5" above. Keep the 0.1 µF + 10 µF at the sensor end.

**Sanity check:** `i2cdetect -y 1` should now show exactly **0x29** (VL53L1X)
and **0x5A** (MPR121) — not 0x48, and no GPIO-based sensors. The 4.7 kΩ
pull-ups stay (still need them for the bus; the VL53L1X breakout's are weak).

**Adding the rest later** is non-destructive — each omitted device just taps
the existing buses: AM312 = 3 wires (VCC/OUT→a free GPIO/GND, no circuitry);
ADS1115 = 4 wires onto the I²C bus (0x48); TLC555 breath = the oscillator
circuit + GPIO17 per the full layout above.

**Backend note:** with this variant the event vocabulary is just `distance_cm`
and `touch_changed(channel, state)` — no `motion_started`/`motion_ended` or
`breath_detected` until those sensors are added.

**Sanity check sequence after wiring:**

1. `sudo i2cdetect -y 1` — should show 0x29, 0x48, 0x5A reliably.
2. Read GPIO4 in a loop and wave a hand — should toggle HIGH/LOW.
3. Frequency-count GPIO17 — should read ~1-2 kHz at room humidity; blow gently on the HR202 and watch it spike.
4. Read VL53L1X — should return a sensible distance to a hand held in front of the sensor.
5. Touch MPR121 electrodes — `mpr[n].value` should reflect touch state per channel.

---

## Backend Architecture Notes

- **Threading:** `pigpio` frequency counting and `gpiozero` PIR callbacks run in their own threads. The I2C polling loop (VL53L1X, MPR121 IRQ confirmation, future ADS1115) should also be its own thread. The visual rendering layer should not be in the same Python GIL-bound context as the sensor polling — use a queue or shared-memory IPC.
- **Event vocabulary** the backend should expose to the visual layer:
  - `motion_started`, `motion_ended` (from AM312)
  - `distance_cm` (continuous stream, ~50 Hz, smoothed)
  - `breath_detected` (discrete event, debounced 3 s)
  - `touch_changed(channel, state)` (discrete event per electrode)
- **Startup discipline:**
  - Wait 60 s before emitting AM312 events.
  - Initialize VL53L1X ranging mode + timing budget before first read.
  - Start the TLC555 baseline tracker in "warming up" mode for ~10 s, accepting only readings as baseline (no breath detection) until it stabilizes.
  - Run `i2cdetect` programmatically at startup; log a warning if any expected device is missing.
- **Failure modes:**
  - I2C device not responding → log + continue with remaining sensors (graceful degradation).
  - GPIO17 frequency stuck at 0 Hz → 555 is not oscillating; log and disable breath detection until reboot.
  - VL53L1X returning `None` for >5 s continuously → likely "no target in cone," not a failure. Returning errors continuously is a failure.

---

## Known Gotchas

- The **AM312 needs 30-60 seconds** to settle after power-up. Don't skip the startup suppression.
- The **VL53L1X cheap breakouts have unreliable I2C pull-ups.** The 4.7kΩ external pull-ups on the bus are not optional in this setup.
- The **TLC555 requires CMOS-grade input impedance** — a bipolar NE555 will not work with R2 in the megohm range. Use the TLC555CP specifically.
- The **timing capacitor must be C0G dielectric.** Y5V/Z5U bulk ceramics drift wildly with temperature and will produce false breath events as the room warms or cools.
- The **HR202's recovery time is slow** (multiple seconds). Don't tie the visual effect duration to sensor decay; use a fixed animation timeline.
- The **MPR121 baseline auto-calibrates** to its environment over the first ~30 seconds after init. Touch behavior may be inconsistent during this window.
- The **Pi's internal I2C pull-ups (~1.8kΩ)** parallel with the external 4.7kΩ to give ~1.3kΩ effective. This is fine and within spec.
