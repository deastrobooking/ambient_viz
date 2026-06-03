# ESP32 Remote Sensor Network — Design Spec (future)

Status: **design only, not built.** This is a forward-looking spec for replacing
the current single-board wired sensor cluster with a network of wireless ESP32
sensor stations, communicating to the Pi without assuming any Wi-Fi
infrastructure. It supersedes nothing yet — the shipping system is the soldered
two-board wired build in [`hardware-handoff.md`](hardware-handoff.md). Read that
first; this doc assumes its sensor set, addresses, and detection semantics.

> **Why consider this.** The wired build pins every sensor to one perfboard a
> short jumper run from the Pi (plus the 2 m Cat5 VL53L1X run). An exhibit that
> wants sensors *spread across a room* — multiple touch points, presence and
> ranging at the entrance vs. at the kiosk face — hits the wiring limit fast.
> ESP-NOW removes the **data** tether. It does **not** remove the **power**
> tether (see [§5](#5-powering-the-remotes)); that's the part that decides
> whether this is worth building.
>
> Thematically this fits [[artistic-statement-pain-material]]: a covert mesh of
> battery nodes quietly reporting presence and touch back to a central host
> *is* a surveillance sensor network. The architecture can carry some of the
> paranoia/surveillance content, not just the visuals.

---

## 1. Topology

```
  ┌─ remote 1 ─┐                                    enclosure
  │ ESP32-C3   │··ESP-NOW··┐                  ┌─────────────────────┐
  │ MPR121     │           │                  │  ┌─ host ESP32 ─┐   │
  └────────────┘           ├··ESP-NOW········►│  │ ESP32-S3/C3  │   │
  ┌─ remote 2 ─┐           │                  │  │ aggregator   │   │
  │ ESP32-C3   │··ESP-NOW··┤                  │  └──────┬───────┘   │
  │ VL53L1X    │           │                  │         │ USB-CDC    │
  └────────────┘           │                  │  ┌──────▼───────┐   │
  ┌─ remote N ─┐           │                  │  │ Raspberry Pi │   │
  │ ESP32-C3   │··ESP-NOW··┘                  │  │ Node bridge  │   │
  │ AM312+555  │                              │  └──────────────┘   │
  └────────────┘                              │     (+ Daisy)        │
   multi-producer ──────────────► single-consumer (host) ──► Pi      │
                                              └─────────────────────┘
```

- **Remotes** (multi-producer): each is an ESP32 carrying one or more sensors,
  transmitting readings over ESP-NOW.
- **Host** (single-consumer): one ESP32 inside the enclosure alongside the Daisy.
  It is the *only* ESP-NOW receiver, aggregates all remote traffic, and forwards
  to the Pi over USB serial.
- **Pi**: unchanged role — runs the existing Node SSE bridge and Python sidecar.
  The serial stream replaces the GPIO/I²C reads; everything downstream of
  `POST /ingest` (see [`SENSOR_MAPPING.md`](SENSOR_MAPPING.md)) is untouched.

The MPSC framing is the contract: every payload the host forwards is tagged with
`node_id` + `seq` so the Pi can demultiplex producers from one serial stream.

---

## 2. Host ESP32 → Pi link

**Decision: USB CDC serial.** The host plugs into a Pi USB port and enumerates
as a serial device; the Node bridge opens it instead of reading hardware
directly. This solves data link *and* host power in one cable.

| Option | Verdict | Why |
|---|---|---|
| **USB CDC serial** | ✅ **chosen** | Cleanest; powers the host; minimal change to the Node bridge (open a port, parse frames). Native USB on S3/C3 = robust CDC-ACM. |
| GPIO UART (Pi ↔ ESP32) | ⚠️ fallback | Works (both 3.3 V, no level shift) but doesn't power the host and the Pi's primary UART is often contended. |
| I²C/SPI slave (Pi master) | ❌ | ESP32-as-I²C-slave is historically twitchy; forces polling. Not worth it. |

**Host part choice: ESP32-S3 or ESP32-C3** specifically, for their *native* USB
peripheral (true CDC-ACM, no CP2102/CH340 bridge chip to flake out). A classic
ESP32 works via its onboard bridge but is strictly worse here.

**Device naming.** Give the host a stable udev rule or fixed product string so
the Node bridge finds it deterministically (`/dev/ttyACM*` enumeration order is
not stable). Compare the Daisy's approach — it uses an explicit USB PID so the
device name is predictable (see commit `d3b92f0`). Do the same here.

**Framing.** Sensor data is tiny and low-rate, so either:
- newline-delimited JSON (`{"n":2,"seq":1041,"distance_cm":118.4}\n`) — trivial
  to debug, fine at these rates; or
- length-prefixed binary + COBS — if you want byte efficiency / robustness.

Every frame carries `node_id` + `seq` + payload. The Node bridge maps each
`node_id` to a logical sensor role and emits the existing event vocabulary
(`distance_cm`, `motion`, `touch_changed`, `breath_detected`).

---

## 3. Is the ESP32 suitable for these sensors?

**Yes — arguably a better fit than the Pi for the sensor I/O**, because it's a
real microcontroller: deterministic timing, hardware counters, no OS jitter.

| Sensor | Bus | ESP32 fit | Notes |
|---|---|---|---|
| **MPR121** | I²C | ✅ | Mature Arduino/ESP-IDF drivers. IRQ → any GPIO. |
| **VL53L1X** | I²C | ✅ | Pololu/Sparkfun/ST libs run on ESP32. Same 0x29 collision rule applies — XSHUT sequencing or a TCA9548A mux for multiples *on one node*. |
| **AM312 PIR** | GPIO | ✅ | Plain digital input. Same 30–60 s power-up settle suppression as the Pi build. |
| **HR202 + TLC555** | freq | ✅✅ | *Better* on ESP32: the 555's pulse output goes to the hardware **PCNT** (pulse counter) or **RMT** peripheral — measures frequency without burning CPU or fighting OS scheduling, unlike `pigpio` edge-counting on Linux GPIO. |

**Constraints to respect:**
- **ADC2 is unusable while the radio is on** — if a node ever needs analog
  (e.g. an ADS1115 replacement or a thermistor breath fast-path), use ADC1 pins
  or keep the ADS1115 on I²C.
- Watch I²C pull-ups / bus loading per node (the cheap VL53L1X breakouts have
  weak pulls — same gotcha as the wired build, add 4.7 kΩ externally).
- One node should carry a *small* sensor set (1–3). Don't rebuild the whole
  cluster on one ESP32 — the point of going wireless is spatial distribution.

**Detection logic is unchanged.** The smoothing, baselines, debounce, and
startup discipline in `hardware-handoff.md` / `SENSOR_MAPPING.md` move from the
Pi sidecar to the node firmware (or stay on the Pi — see [§6](#6-where-does-detection-logic-live)).

---

## 4. ESP-NOW MPSC protocol

ESP-NOW is a **connectionless single-hop link layer** — no Wi-Fi AP, no IP, no
association handshake. That's exactly what "no Wi-Fi network" needs.

- **Pairing:** remotes send unicast to the host's fixed MAC. Host registers each
  remote as a peer (≤ 20 encrypted peers; more if unencrypted/broadcast).
- **Reliability for free-ish:** ESP-NOW unicast gets a **link-layer ACK**, so
  the MPSC layer mostly needs **`node_id` + `seq`** rather than a full reliable
  transport. Add an application retry only for payloads that must not drop
  (e.g. a touch *release* — a lost release sticks a channel on).
- **Channel:** all nodes must share one fixed Wi-Fi channel (pick one, hardcode
  it; no scanning since there's no AP).
- **Collisions:** the Wi-Fi MAC does CSMA, but under contention (several remotes
  firing at once) add small **per-node random jitter** to spread bursts. Vary
  jitter by `node_id` so nodes don't sync up.
- **Payload cadence:** continuous streams (distance ~50 Hz) vs. discrete events
  (touch/breath/motion). Rate-limit the continuous ones at the node (the Pi only
  needs ~50 Hz smoothed; see `VL53_SMOOTH_ALPHA`) to keep the channel clear for
  event traffic.

Suggested frame fields: `node_id`, `seq`, `type` (stream|event), `payload`,
optional `batt_mv` (for the power-monitoring story in [§5](#5-powering-the-remotes)).

---

## 5. Powering the remotes

This is the part that decides whether the whole idea is worth it.

> **Both batteries you'd reach for first are wrong:**
>
> - **Coin cell (CR2032): no.** Two independent failures. Capacity is ~225 mAh,
>   but the killer is internal resistance — a CR2032 can't source the
>   ~100–250 mA burst the radio draws on TX without the rail sagging into
>   brownout. Even with deep-sleep duty cycling (deep sleep ~10 µA; ESP-NOW's
>   connectionless model lets you wake→read→send→sleep with no association), the
>   *peak* TX current still browns it out. Coin cells suit radios *designed*
>   around them (BLE beacons with big reservoir caps), not a general ESP32.
> - **9 V battery: also no.** ~550 mAh, high internal resistance, and you waste
>   a chunk as heat dropping 9 V → 3.3 V. Poor energy density for sustained
>   hundreds of mA.

**What actually works:**

| Supply | Verdict | Notes |
|---|---|---|
| **Wired 5 V / DC bus** | ✅ best for a permanent exhibit | Eliminates the maintenance chore entirely. If a node is within a few meters, run power even if data is wireless. |
| **USB power bank (per node)** | ✅ pragmatic | Cheap, large capacity, hot-swappable, no charge circuit to design. |
| **Li-ion 18650 / LiPo + charger** | ✅ if truly untethered | ~2500–3500 mAh; onboard TP4056-class charging + 3.3 V reg. ~30 h at ~80 mA continuous; far longer if duty-cycled. |
| 9 V / coin cell | ❌ | See callout above. |

> **The core tension.** Deep-sleep duty cycling is what makes batteries last —
> but your sensors are *interactive*. Touch (MPR121) and presence/ranging
> (VL53L1X) need low-latency continuous reporting, which fights aggressive
> sleep. So an interactive node is pushed toward a *real* battery (Li-ion) or
> wired power, and a Li-ion node means a recurring recharge/replace chore for an
> exhibit running an 18-min loop all day (see [[exhibit-composition-structure]]).
>
> **Recommendation:** for a permanent install, prefer **wired power** to the
> remotes and let ESP-NOW carry only data. Reserve true battery operation for
> nodes that genuinely can't be reached by a wire. If on battery, report
> `batt_mv` in every frame so the Pi can surface a low-battery warning before a
> node dies mid-show.

---

## 6. Range and multi-hop

ESP-NOW does ~200 m line-of-sight; much less indoors through enclosures/metal.
For one room it's fine. If range is marginal:

**Do this first — fix the antenna, not the protocol.** Indoor range problems are
usually PCB-antenna boards sitting in an enclosure's RF shadow. An
**external-antenna (U.FL/IPEX) ESP32 on the host**, or simply repositioning the
host out of the metal, usually recovers the range you'd otherwise chase with
hops — with zero protocol risk.

**If you genuinely need multi-hop:** ESP-NOW has *no* native routing — you build
it. Two paths:

1. **Controlled flooding on raw ESP-NOW** (~100 lines): a node that can't reach
   the host broadcasts; any node that hears it re-broadcasts. Every packet
   carries `(origin_node_id, seq, ttl)`; each node keeps a **dedup cache** of
   recently-seen `(origin, seq)` to kill loops/storms; decrement TTL per hop.
   Simple, but floods — doesn't scale past a few hops before channel contention
   bites.
2. **ESP-MESH** (`esp-mesh-lite` / `painless-mesh`): real self-healing
   multi-hop trees, but built on the **Wi-Fi stack**, not bare ESP-NOW —
   heavier RAM/CPU/power and more complexity than telemetry warrants.

> **The catch that usually kills hopping here:** a relay node must keep its radio
> **awake** to forward — directly incompatible with the deep-sleep that made
> batteries viable. So don't make battery sensor nodes relay.
>
> **If you need a hop, add a dedicated repeater:** one extra ESP32 whose only job
> is relay, placed at the range midpoint and **mains/USB-powered** (it's awake
> anyway). Sensor remotes stay dumb single-hop senders; the repeater
> store-and-forwards toward the host. Far easier to reason about than
> peer-to-peer flooding, and it keeps the battery nodes sleepy.

It's the **power topology**, not the data topology, that constrains this design —
that theme recurs throughout this spec.

---

## 7. Where does detection logic live?

Two viable splits; pick per sensor:

- **On the node** (smoothing/baseline/debounce in firmware, send events):
  lowest channel traffic, best for battery (send only on change), but tuning
  knobs (`VL53_SMOOTH_ALPHA`, breath threshold, AM312 60 s suppression) now live
  in firmware — slower to iterate, needs reflash.
- **On the Pi** (node sends raw-ish readings, Pi runs existing logic): keeps all
  the tuning in the Python sidecar / `config.py` where it already is and is
  documented; costs more channel traffic and battery.

Recommended default: **continuous streams (distance) lightly pre-smoothed on the
node** (to throttle channel rate) with final mapping on the Pi; **discrete events
(touch/breath/motion) fully detected on the node** (send only the event). This
matches the battery-friendly "send on change" model while keeping the
distance-curve tuning on the Pi where `SENSOR_MAPPING.md` documents it.

---

## 8. Open questions / next steps

- [ ] Confirm node count and physical placement (drives wired-vs-battery per node).
- [ ] Prototype: one ESP32-C3 remote (VL53L1X) → one ESP32-S3 host → Pi over
      USB CDC; verify the Node bridge can parse framed multi-node traffic.
- [ ] Measure real indoor ESP-NOW range in the actual enclosure *before* deciding
      anything about repeaters.
- [ ] Decide detection-logic split per sensor ([§7](#7-where-does-detection-logic-live)).
- [ ] If battery: pick Li-ion vs. power-bank, add `batt_mv` telemetry + Pi
      low-battery surfacing.
- [ ] udev rule / fixed USB descriptor for deterministic host enumeration.

## See also

- [`hardware-handoff.md`](hardware-handoff.md) — current wired build; sensor
  pinouts, addresses, detection semantics (the source of truth this spec reuses).
- [`SENSOR_MAPPING.md`](SENSOR_MAPPING.md) — sensor → visualizer pipeline; nothing
  downstream of `POST /ingest` changes under this design.
- `daisy/` — the other in-enclosure coprocessor ([[daisy-coprocessor-arch]]); the
  host ESP32 shares the enclosure and the "talk to the Pi over USB" pattern.
