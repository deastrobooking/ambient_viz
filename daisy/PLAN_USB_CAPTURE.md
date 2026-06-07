# PLAN: Reviving live USB audio capture (visualizer feed)

Design notes from the 2026-06-06 analysis session. Context: the Daisy streams
its line-out to the Pi over USB so the browser visualizer can run FFT/envelope
analysis on it. That live capture was abandoned for the exhibit in favour of
`localaudio` mode (the browser analyses the pristine master mp3 seeked to the
Daisy `POS` — see `static/index.html` and EXHIBIT.md). This doc records *why*
it failed, what would fix it, and the recommended direction, so we don't
re-derive it next time.

Related: `PLAN_USB_COMPOSITE.md`, `PLAN_QSPI_BOOTLOADER.md`,
`crates/firmware/src/{usb_audio,uac_source}.rs`, `crates/firmware/src/main.rs`.

## Two distinct failures (do not conflate)

The capture path has **two separate problems** with different causes and fixes:

- **(a) Daisy-side clicks — minor.** The firmware UAC1 source streams audio over
  a Full-Speed **isochronous** IN endpoint, polled by the Pi host every **1 ms**
  (the FS USB frame; `bInterval=1`). The audio task and the SD-card reader share
  one embassy thread executor (USB futures are `!Send`, so USB cannot be moved to
  its own interrupt executor the way the audio path was). A blocking SD read
  freezes the executor past the 1 ms iso deadline → the stream task can't stage a
  packet → missed poll → click. *Tolerable* — the analog line-out the audience
  hears is clean; the capture only drives FFT.

- **(b) Pi-side clocking — the actual showstopper.** Under Chromium's
  `getUserMedia` → PipeWire graph-scheduled capture, the `alsa_input…Daisy` /
  "Chromium Input" nodes sit at **rate 0**, twitch, and disappear — the capture
  is mostly silence. Crucially, **`pw-record` captures the same device fine**, so
  the Daisy *is* delivering a usable stream; the failure is PipeWire failing to
  keep the graph-scheduled capture node clocked for Chromium. This is what made
  browser capture unusable and forced `localaudio`.

**A Pi-side tweak can only ever help (b). Nothing on the Pi fixes (a).**

## Diagnostic instrumentation (already in firmware, 2026-06-06)

To settle which failure dominates — our notes said the cause was never confirmed
— the firmware now exports two per-interval counters on the `debug-uart` `diag:`
heartbeat (`crates/firmware/src/main.rs`, `usb_audio.rs`):

- `usb_drop` — samples the SAI→USB tee dropped because the iso drain couldn't
  keep up (USB_RING full). Counted **only while actively capturing**
  (`USB_CAPTURING` gate) so the parked/idle overflow doesn't pollute it. High ⇒
  **overflow** mode (SD stalls outrun the ~5.3 ms ring; a bigger ring would help).
- `usb_pkt_max_fr` — largest single-poll drain in stereo frames. ~48 = healthy
  1 ms pacing; climbing toward the 56-frame packet cap ⇒ **catch-up after missed
  polls** (scheduling; only async SD or a wider interval helps — a bigger ring
  would NOT).

Read together each beat: `drop≈0 & pktmax≈48` = clean; `drop` spiking = overflow;
`pktmax` pinned near the cap with low `drop` = missed polls.

## Options considered (and why)

| Option | Fixes (a)? | Fixes (b)? | Cost | Verdict |
|---|---|---|---|---|
| **Bigger USB_RING** (512→2048 i16, ~5.3→21 ms) | No (scheduling, not capacity; drain rate fixed at +17% by the 224 B packet cap) | No | 1-line | Only helps overflow; a typical sub-few-ms SD read doesn't overflow the current ring. Diagnostic value only. |
| **2 ms iso interval** (`bInterval=2`) | Partially — doubles the slack before a poll is missed | No | small fw | Fewer but **bigger** glitches (2 ms lost per miss); halves catch-up (double `MAX_EXTRA_SAMPLES`); doubles latency; FS hosts may dislike non-1 ms iso. Note in `uac_source.rs`. |
| **async/DMA SD** | **Yes** (executor never freezes) | No | big embassy rework (no off-the-shelf async FAT) | Real root-cause fix for (a) but leaves you on `getUserMedia`, so (b) remains. |
| **Audio/features over Ethernet (UDP)** | Yes (off the iso deadline) | Yes (bypasses capture stack) | **High** — no PHY on Daisy Seed, embassy-net won't fit the 128 KB flash (needs QSPI first), UDP→browser bridge | Dominated by WebUSB below. |
| **Send analysis features over existing CDC/UART** | N/A (no raw audio) | Yes | moderate fw (onboard FFT, tight H750) | Cheap; loses raw-audio flexibility on the Pi. Good fallback. |
| **Pi-side RT tweaks** (rpi-usb-audio-tweaks-style) | No | Maybe, if (b) is scheduling starvation | low–moderate | Re-target RT priority/affinity + service-stripping at **PipeWire/wireplumber/Chromium**, not MPD/Roon. Static-IP & RT-patched kernel **not** worth it on the kiosk. |
| **Disable USB autosuspend + PipeWire quantum/clock config** | No | Likely | trivial–low | **Not in that repo**; highest-value cheap Pi-side moves. "Rate 0" on an async UAC source may be clock-negotiation, not scheduling. Try first. |
| **WebUSB via vendor BULK endpoint** | **Yes** (bulk has no fixed deadline + ACK/retransmit → stalls delayed, not dropped) | **Yes** (bypasses PipeWire/Chromium capture graph entirely) | moderate | **Recommended.** See below. |

## Recommended direction: WebUSB over a vendor BULK endpoint

**You cannot WebUSB the existing UAC stream** — audio is a WebUSB-blocklisted
device class, the kernel `snd-usb-audio` driver owns the interface, and WebUSB's
isochronous support is poor/unreliable on Linux.

Instead, re-define the Daisy USB so audio leaves over a **vendor-specific
(class 0xFF) interface on a BULK IN endpoint**, read from the page via
`navigator.usb` → `requestDevice` → `claimInterface` → `transferIn` loop →
parse PCM → AudioWorklet ring → the existing AnalyserNode graph.

Why it's better:
- **Fixes (b) by construction** — no `getUserMedia`, no PipeWire graph-scheduled
  capture node, no clock-follower negotiation. Same reason `pw-record` worked.
- **Largely fixes (a)** — bulk has no 1 ms deadline and has ACK/NAK +
  retransmission. A blocked executor just delays the transfer; data is delivered
  late, not dropped. Same "escape the iso deadline" benefit as Ethernet, but with
  *guaranteed delivery* and over the existing cable.
- **Cheaper than Ethernet** — no PHY, no smoltcp/flash blowup, no UDP bridge.
  The vendor bulk descriptor is likely *smaller* than the hand-rolled UAC1 code
  (`uac_source.rs`, ~520 lines) → possibly flash-neutral or a win vs the 128 KB
  ceiling. WebUSB needs no device-side stack (the BOS/WebUSB descriptor is
  optional; user just picks the device from the chooser).

Costs / caveats:
- Firmware descriptor rework: vendor interface + bulk IN endpoint, re-point the
  SAI tee at it. CDC (POS) stays kernel-owned and coexists in the composite.
- Daisy stops being a system audio *input* (fine — analog line-out is the codec,
  untouched; we only used capture for analysis).
- Browser-side PCM reconstruction (USB I/O in JS → AudioWorklet) instead of a
  ready-made MediaStream. Downstream analyser graph unchanged.
- Kiosk needs a secure context (localhost OK) + WebUSB pre-authorization
  (`WebUsbAllowDevicesForUrls` enterprise policy) so it auto-connects with no
  user gesture.
- Bulk bandwidth isn't *guaranteed*, but ~192 KB/s on a 12 Mbit/s FS bus with one
  device + CDC is a non-issue; the receiver buffers.

## Decision gate

This only matters if we want **live** capture back (so the visuals track the
Daisy's interaction-modified output) over the higher-fidelity-but-static
`localaudio`. Before any rework: flash `debug-uart`, briefly revive
`getUserMedia` capture, and read `usb_drop`/`usb_pktmax`:
- counters clean but Pi still can't capture ⇒ pure (b) ⇒ Pi-side quick wins
  (autosuspend off → PipeWire quantum/clock → RT priority) may be enough.
- counters show misses ⇒ (a) is real ⇒ WebUSB-bulk (or async SD) is the path.
