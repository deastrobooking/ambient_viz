# Plan: Composite USB device — UAC1 source + CDC ACM (position out + sensor-MIDI in)

Single USB cable from Daisy to Pi enumerates as both:
- **UAC1 source** — audio capture device for `getUserMedia` in the visualizer
- **CDC ACM** — serial port (`/dev/ttyACM0`), **full-duplex**:
  - device → host: song-position (`POS` / `RESET`) lines
  - host → device: sensor **MIDI Control Change** frames (Phase E)

Goal: Pi's visualizer can read the JSON lanes at the same logical position
the Daisy is rendering, with no cross-cable drift. Same USB SOF clock owns
both streams.

Second goal (Phase E): the Pi senses proximity **once** (the existing
VL53L1X) and drives **both** the visuals (SSE → `applyAutomation`) and the
Daisy's audio knobs (this cable, host → device) off that one number. There is
no second distance sensor and nothing to keep in sync — the audio dissolve and
the visual glitch ride the same reading by construction. See "Phase E" below.

## Current state

- Firmware is blinky-level (`crates/firmware/src/main.rs`).
- SD card scaffolding in `crates/firmware/src/sd.rs`, not yet driven.
- No audio path, no USB device path, no codec init.
- `daisy-embassy`'s `examples/usb_uac.rs` is the canonical reference for
  composite-with-IADs setup (it already sets `composite_with_iads = true`).
- Pi has an existing SSE bridge for kiosk sensors that the position channel
  can piggyback on.

## Implementation order

### Phase A — Codec + audio rendering (prerequisite)

1. Initialize the Daisy Seed 1.2's PCM3060 codec via `daisy-embassy`'s
   `prepare_interface` (mirror `examples/passthrough.rs`).
2. Wire `dsp::Engine::process()` into the audio callback. Engine renders
   the kick+hats+sampler-from-SD chain; output goes to the codec DAC.
3. Verify by ear: drums + (silent if SD not yet wired) backing track.

### Phase B — UAC1 source class only

1. New module `crates/firmware/src/usb_audio.rs`.
2. Construct `embassy_usb::Builder` exactly as in `daisy-embassy`'s
   `examples/usb_uac.rs`, but use `embassy_usb::class::uac1::source::Source`
   instead of `speaker::Speaker`. Source = Pi sees us as a microphone.
3. Three embassy tasks (lifted from the example):
   - `usb_task` — runs `usb_device.run()`
   - `usb_streaming_task` — pulls samples out of an `embassy_sync::channel`
     and writes them to the iso IN endpoint
   - `usb_feedback_task` — sends sample-rate feedback every 8 frames
4. The audio task in Phase A also writes the same samples into the channel
   that the streaming task reads from. (Double-pumping: codec DAC + USB).
5. Verify on Pi: `arecord -l` shows the device, `arecord -D plughw:N,0
   -c 2 -r 48000 -f S32_LE -d 5 /tmp/t.wav` produces a non-silent file.

### Phase C — Add CDC ACM to the composite

1. New module `crates/firmware/src/usb_cdc.rs`.
2. In the same `Builder`, before `builder.build()`, add a
   `CdcAcmClass::new(...)`. Allocates 2 endpoints (1 bulk IN, 1 bulk OUT)
   plus a notification endpoint.
3. Two new embassy tasks:
   - `cdc_class_task` — runs the class's internal loop
   - `position_emit_task` — every 50 ms, format
     `"POS {seconds_in_loop:.3}\n"` and call `cdc.write_packet(...)`.
4. Confirm `composite_with_iads = true` is set on the device config
   (it already is in the reference example).
5. Verify on Pi: `ls /dev/ttyACM*` shows the new port, `cat
   /dev/ttyACM0` prints `POS 0.001`, `POS 0.051`, …

### Phase D — Pi bridge + browser consumer

1. Node tail process: open `/dev/ttyACM0`, line-buffer, parse `POS T`
   and `RESET T` messages, publish to existing SSE bridge under a new
   topic `song_position`. Auto-reopen on EOF / `ENOENT`.
2. Visualizer subscribes to the topic. Stores `(last_reported_pos, wall_time_received)`.
3. Per-frame: `effective_pos = last_reported_pos + (now - wall_time_received) * 1.0`
   (the `1.0` is playback rate; will become a separate signal if we ever
   support tape-failure-driven slowdown).
4. Replace the visualizer's current "audio time" with `effective_pos` for
   all `lanes.*` lookups.

### Phase E — Inbound sensor → MIDI channel (distance → coupled audio + visual)

**Why this and not a second sensor.** The Pi already produces one smoothed
`distance_cm` and already fans it to the visuals. To make an audio effect that
is *tied to* the visual effect, both must read the **same** number — two
independent distance sensors would drift, disagree on noise/latency/aim, and
need reconciliation. So distance stays single-source on the Pi and we add the
audio leg over the cable we're already running. (A second ToF only earns its
place for *distinct zones* — EXHIBIT.md ideas G/H — which is a different
feature, not coupling.)

**Endpoint cost: zero.** CDC ACM is full-duplex; Phase C already allocated the
host → device **bulk OUT** endpoint and we simply weren't reading it. Phase E
just starts reading it. Re-confirm complication #1's count is unchanged.

**Wire format: raw 3-byte MIDI Control Change frames** `[0xB0|ch, cc, value]`.
This reuses `dsp::midi::decode` *verbatim* — the inbound channel is literally
"MIDI tunneled over CDC," symmetric with the planned TRS-UART MIDI path. It
lands in the engine through the exact code the hardware-controller path uses:

```
CDC OUT bytes → MidiByteParser → midi::decode → Engine::handle_midi
             → MidiMap::map_cc → Engine::apply_param   (all already implemented)
```

No new parser, no new dispatch, no new `Param`. Rejected alternative: text
lines (`CC 23 64\n`, symmetric with the `POS` text out). It reads nicely but
needs a *second* parser and forfeits the `decode()` reuse; the binary frame is
still hand-debuggable (`printf '\xB0\x17\x7F' > /dev/ttyACM0`).

#### Daisy side — `crates/firmware/src/usb_cdc.rs`

1. New task `cdc_midi_in_task`: loop `cdc.read_packet(&mut buf).await`, feed the
   bytes to a `MidiByteParser` that yields complete `MidiMessage`s.
2. Push each decoded message onto an `embassy_sync::channel::Channel<_,
   MidiMessage, N>` (it's `Copy` and tiny). The **audio task** drains this
   channel at the top of each `process()` block and calls
   `engine.handle_midi(msg)`. This keeps `handle_midi` off the USB task and off
   any lock on the RT path — the lock-free mirror of how the host marshals via
   `Arc<Mutex<Engine>>`.
3. At startup, install the kiosk CC→Param bindings on the engine's `MidiMap`,
   **identical to the host**. Factor the host's `bind_cc` block
   (`crates/host/src/main.rs:104-113`) into a shared
   `dsp::install_kiosk_bindings(&mut MidiMap)` so host + firmware can't drift.

`MidiByteParser` (new, in `crates/dsp/src/midi.rs`, `no_std`): a small
status+data accumulator. CDC hands us an *unframed* byte stream, but MIDI is
self-framing — status bytes have bit 7 set, data bytes don't — so we buffer
from a status byte until the expected data-byte count is present, then call
`midi::decode`. (`midi.rs` already notes the decoder "expects complete
status+data byte sequences per call"; this parser is what produces them.) The
**TRS-UART MIDI input needs exactly the same accumulator**, so build it once
and share it across both transports.

#### Pi side — Node bridge (reuse the Phase D fd; do not open a 2nd owner)

The bridge already opens `/dev/ttyACM0` to tail `POS`, and already receives
`distance_cm` via `POST /ingest`. So the *same process, same fd* writes the
inbound CC frames — no new serial owner, no contention.

1. On each `distance_cm` ingest, map distance → a CC value with the inverted
   "presence" curve and write a CC frame to the serial fd. The artistic
   shaping lives **here** (tunable without reflashing), next to the existing
   visual distance curve; the Daisy's `MidiMap` stays a dumb linear
   `0-127 → [min,max]`.

   ```js
   const NEAR = 25, FAR = 100;                 // cm — mirrors VL53_NEAR_CM / VL53_FAR_CM
   const clamp = (x,a,b) => Math.min(b, Math.max(a, x));
   // near = pristine & present (visual twist at max); far = tape eaten (twist → 0)
   const failure = clamp((d - NEAR) / (FAR - NEAR), 0, 1);
   const cc = Math.round(failure * 127);
   port.write(Buffer.from([0xB0, 23, cc]));    // CC 23 = TapeFailure (host binding)
   ```

   Both the audio `TapeFailure` and the visual `maxTwistDeg` are now functions
   of the one `distance_cm` — that *is* the coupling. They don't share a curve
   (failure rises with distance, twist falls with it); they share a *source*.
2. **Dedupe + throttle**: only `write` when the mapped `cc` changed; cap to
   ≤30 Hz. `distance_cm` is already EMA-smoothed and 0.1 cm-quantized upstream,
   so most ticks collapse to the same `cc` and never hit the wire.
3. Optional layering (EXHIBIT.md tier 1, "process the finished mix"): fan the
   same distance to `ReverbWet` / `StabDelayWet` as additional CCs (CC 21 is
   already bound to `ReverbWet`; add bindings for the delay). Same mechanism,
   more CCs — presence opens up *space* as well as resolving the tape.

#### Dev/host parity (testable today, no Pi)

The macOS `host` already routes **CC 23 → TapeFailure** from any hardware MIDI
controller (`main.rs:113`). So the entire *dsp* side of this feature works now:
turn a knob, hear the tape eat itself. Phase E adds only the transport — the
audio behavior is already exercisable and unit-testable without hardware.

## Complications

1. **Endpoint count.** STM32H7 OTG_FS has 9 IN + 9 OUT endpoints (minus
   EP0). UAC1 source uses 2 IN (iso audio + feedback), CDC uses 1 IN
   bulk + 1 OUT bulk + 1 IN interrupt. Total 4 IN + 1 OUT. Well within
   budget but worth re-checking when the descriptor goes live —
   `embassy-usb` will fail at build time if oversubscribed.
2. **Composite-with-IADs descriptor**. `bDeviceClass=0xEF`,
   `bDeviceSubClass=0x02`, `bDeviceProtocol=0x01` must be set. The
   `usb_uac.rs` reference sets these via `composite_with_iads = true`
   — keep that line, don't override device class manually.
3. **Position source must come from the audio path, not a separate timer.**
   The Daisy's internal SAI clock and embassy_time's system timer can
   drift relative to each other (~20 ppm worst case = 12 ms over 10 min).
   The position-emit task should read `Engine::sequencer().time_seconds()`,
   which advances per *audio sample rendered*, not per wall-clock tick.
4. **CDC bulk endpoint can stall if the host stops reading**.
   `write_packet` returns `EndpointError::Disabled` if the pipe is
   closed; the emit task must catch that, drop the message, and retry
   on the next tick. Don't block.
5. **Hot-plug resilience**.
   - Pi side: Node bridge must reopen the serial port on `ENOENT` /
     read errors. Loop with 1 s back-off.
   - Browser side: SSE auto-reconnects, but the visualizer should
     handle the "stream paused, then resumed at a different position"
     case — hard-snap rather than interpolate across the gap.
6. **Pi audio card numbering**. ALSA assigns card numbers in
   enumeration order. If multiple USB audio devices are on the same
   hub, the Daisy might come up as `card 1` or `card 2`. Add a `udev`
   rule keying on the Daisy's USB VID:PID so it gets a stable name
   (`/dev/snd/by-id/usb-..._Daisy_audio`).
7. **Composite enumeration on macOS dev workstation**. macOS treats
   composite UAC + CDC fine, but the audio card may show up only after
   the user grants microphone permission to whatever app is testing.
   Document this in the dev workflow.
8. **First-message timing in the visualizer**. The visualizer might
   start before the Daisy starts streaming. Until the first `POS`
   message arrives, freeze the lane lookups (or show a "waiting" state)
   — don't fall through to a clock-based default that would then jump.
9. **CDC buffer sizing**. Default `embassy-usb` CDC ACM uses a 64-byte
   packet size. Our messages are ~20 bytes each, well under that. No
   need to tune.
10. **Reset/loop wrap signalling**. When `time_seconds` wraps, the emit
    task should send `RESET 0.000\n` once before resuming `POS …`
    messages. The browser uses this to hard-snap rather than try to
    interpolate across the wrap.
11. **Single owner of `/dev/ttyACM0`** *(Phase E)*. CDC is full-duplex,
    but a tty has one clean reader and one clean writer. The Node bridge
    must own *both* directions (POS read + CC write) on the one fd. Do
    **not** start a second process (e.g. a Python serial writer in the
    kiosk) on the same port — concurrent writers interleave bytes and
    corrupt MIDI frames. The Python kiosk stays unchanged; distance
    reaches the bridge via the existing `/ingest` POST.
12. **Inbound MIDI must not touch the RT audio path under lock** *(Phase
    E)*. Decode on the USB task; hand the `MidiMessage` to the audio task
    via an `embassy_sync::channel` and drain it at block boundaries.
    Never call `handle_midi` / grab the engine from the USB task — that
    would put USB latency on the audio callback.
13. **CC flooding** *(Phase E)*. A 50 Hz distance stream × several params
    can swamp the bulk-OUT pipe and the param smoothers. Send only on a
    *changed* CC value, cap the rate (≤30 Hz), and lean on the upstream
    EMA + 0.1 cm quantize that already collapse most ticks.
14. **Binding drift, host vs firmware** *(Phase E)*. The CC→Param map
    must be byte-identical on Mac dev and on the Daisy or the same
    gesture means different things in the two environments. Share one
    `dsp::install_kiosk_bindings(&mut MidiMap)` and call it from both;
    delete the inline `bind_cc` block in `host/src/main.rs`.

## Out of scope

- **Arbitrary command / RPC channel Pi → Daisy.** Phase E uses the
  inbound CDC direction, but only to tunnel MIDI CC/note bytes through
  the *existing* engine path. A general command protocol (config pushes,
  file transfer, control verbs) is still out of scope — add later if a
  real need appears.
- **USB MIDI device class.** Phase E tunnels MIDI *bytes* over the CDC
  ACM we already have; we do **not** expose a USB-MIDI class. Same
  musical result, no extra interface/endpoints, and the TRS-UART
  controller input is untouched.
- Compressed audio over CDC. Unnecessary when UAC is doing it.
- Multiple visualizer clients. SSE supports this natively if we ever
  want it.

## Files touched

```
crates/
├── firmware/
│   ├── Cargo.toml            # add embassy-usb feature flags if not already
│   └── src/
│       ├── main.rs           # spawn all USB tasks; install_kiosk_bindings()
│       ├── usb_audio.rs (new)  # UAC1 source class + audio iso task
│       ├── usb_cdc.rs (new)    # CDC ACM: POS emit (out) + MIDI-in task (Phase E)
│       └── (existing sd.rs)
├── dsp/src/
│   ├── midi.rs               # add MidiByteParser (shared: CDC-in + TRS-UART)
│   └── midi_map.rs (or lib.rs) # add install_kiosk_bindings(&mut MidiMap)
└── host/src/
    └── main.rs               # replace inline bind_cc block with install_kiosk_bindings()
```

Pi side (existing repo at `~/repos/ambient_viz`):
- `server/sse-bridge.js` (or equivalent) — add a CDC tail transport (POS in)
  **and** a CC writer (Phase E): on `distance_cm`, map → CC and write a
  `[0xB0, cc#, value]` frame to the same fd
- `static/visualizer/index.html` (or wherever) — subscribe to
  `song_position` topic, replace internal clock with `effective_pos`
- Python kiosk (`python/ambient_kiosk/`) — **unchanged**; it already
  publishes `distance_cm` through `/ingest`

## Verification checklist

- [ ] `arecord -l` lists Daisy as an audio capture device
- [ ] 5 s `arecord` produces a non-silent WAV
- [ ] `ls /dev/ttyACM0` exists when Daisy is plugged in
- [ ] `cat /dev/ttyACM0` shows 20 `POS` lines per second
- [ ] Visualizer lane changes line up with audible song events (e.g.,
      a `sliceTrigger` keypoint at t=74.5 fires within ~50 ms of the
      audible cue in the MP3)
- [ ] Unplug + replug recovers within 2 s (both audio and position)
- [ ] Visualizer hard-snaps on loop wrap instead of glitching
- [ ] *(Phase E)* `printf '\xB0\x17\x7F' > /dev/ttyACM0` audibly eats the
      tape (CC 23 = 127) with no Pi sensor in the loop
- [ ] *(Phase E)* moving a hand toward the sensor resolves the captured
      audio from eaten → pristine; pulling away dissolves it
- [ ] *(Phase E, the coupling test)* visual twist and audio failure move
      together off one hand — they never disagree, because there is one
      `distance_cm`
- [ ] *(Phase E)* idle (no target → `VL53_FAR_CM`) holds full
      `TapeFailure` without CC chatter on the wire (dedupe working)
