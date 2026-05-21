# Daisy Seed → Raspberry Pi over I²S (visualizer audio source)

End-to-end guide for getting Daisy Seed audio into the browser visualizer
running on a Raspberry Pi 4, using I²S over GPIO as the transport.

**Honest expectation:** this is an evening project, not a 30-minute one.
The wiring is trivial; the configuration on both sides has sharp edges
(clock master/slave, sample-rate agreement, device-tree overlays). If
you'd rather avoid that, see "Easier alternatives" at the bottom.

---

## Signal flow

```
[Daisy Seed]                          [Raspberry Pi 4]
  STM32H750 ──┐                         ┌── I²S RX peripheral
              │  BCLK  ───────────────► │   (GPIO 18 / PCM_CLK)
   SAI block  │  LRCLK ───────────────► │   (GPIO 19 / PCM_FS)
  (I²S master)│  SDOUT ───────────────► │   (GPIO 20 / PCM_DIN)
              │  GND   ───────────────► │   (any GND pin)
              └─                        └── ALSA capture device
                                            │
                                            ▼
                                          Chromium
                                          getUserMedia
                                            │
                                            ▼
                                          AnalyserNode → visualizer
```

Daisy is the **I²S master** (it already clocks its own onboard codec, so
generating BCLK/LRCLK is its natural mode). Pi is the **slave** (its
I²S peripheral consumes whatever clocks arrive).

Direction is Daisy → Pi only. No return path needed for the visualizer.

---

## Hardware shopping list

| Item | Why | Notes |
|---|---|---|
| Raspberry Pi 4 Model B (any RAM) | Host for visualizer + I²S RX | 8GB is overkill for this workload but fine |
| microSD card, 32 GB+ | Pi OS | Class 10 / A1 minimum |
| Pi 4 USB-C power supply (official 5.1V/3A) | Stable rails matter for clean I²S | Underpowered supplies cause weird ALSA underruns |
| HDMI display + micro-HDMI cable | Run Chromium fullscreen | Or use VNC and accept the latency |
| Daisy Seed (you have one) | Audio source | Either codec revision works |
| 4× female-to-female jumper wires | BCLK, LRCLK, SDOUT, GND | Keep them short — 10 cm or less |
| Optional: small breadboard | Tidier than dangling jumpers | Not strictly needed |
| Optional: logic analyzer or scope | Hugely speeds up debugging clocks | Cheap USB analyzers ($15) are fine |

**Total new cost if you have the Pi and Daisy:** ~$5 in jumpers.

---

## Wiring

Daisy Seed and Pi 4 are both 3.3V logic — **no level shifter needed**.
Both sides must share ground.

Standard Pi 4 I²S input pins (40-pin header):

| Pi 4 header pin | GPIO | Function | Connect to |
|---|---|---|---|
| 12 | GPIO 18 | PCM_CLK (BCLK in) | Daisy SAI BCLK |
| 35 | GPIO 19 | PCM_FS (LRCLK in) | Daisy SAI LRCLK |
| 38 | GPIO 20 | PCM_DIN (data in) | Daisy SAI SDOUT |
| 6 (or any GND) | — | GND | Daisy GND pad |

**Daisy side pins** depend on which SAI peripheral you expose. The
onboard codec already uses one SAI, so you'll either:

- **(A)** Repurpose a second SAI peripheral and route it to header
  GPIOs (cleanest; see "Daisy firmware" below).
- **(B)** Tap the existing codec-bound I²S TX line via test points (no
  firmware changes, but requires fine SMD soldering).

Most people should do (A). Consult the official **Electrosmith Daisy
Seed pinout PDF** to identify which header pins map to SAI alternate
functions on the STM32H750. Don't guess from memory — alternate-
function tables on STM32 are dense and easy to misread.

**Wire-length rule:** at 48 kHz / 32-bit stereo, BCLK is ~3.07 MHz.
That's slow for I²S but fast enough that 30 cm of unshielded jumper
wire starts producing reflections and missed bits. Keep all four wires
short and bundled.

---

## Daisy firmware

Goal: configure a second SAI peripheral as I²S master, mirror the
audio-callback output to it, and route its BCLK/LRCLK/SDOUT to three
GPIOs reachable on the header.

**libDaisy** (the C++ HAL Electrosmith maintains) exposes SAI config
through its audio driver. The official Daisy examples include patterns
for multi-SAI setup — `DaisyExamples/seed/` is the reference. Look
specifically for examples that initialize `daisy::SaiHandle` directly
rather than relying on the default `hw.Init()` codec path.

Skeleton (pseudocode — fill in the exact API from libDaisy headers,
which evolve):

```cpp
DaisySeed hw;
SaiHandle sai_out;  // second SAI, header-pin output

void AudioCallback(AudioHandle::InputBuffer in,
                   AudioHandle::OutputBuffer out, size_t size) {
    for (size_t i = 0; i < size; i++) {
        float s = /* your synth/sample/whatever */;
        out[0][i] = s;
        out[1][i] = s;
        // Write the same sample to the second SAI's TX buffer.
        // Use sai_out.GetTxBuffer() / interleave format per libDaisy.
    }
}

int main(void) {
    hw.Init();

    SaiHandle::Config sai_cfg = {};
    sai_cfg.periph         = SaiHandle::Config::Peripheral::SAI_2;
    sai_cfg.sr             = SaiHandle::Config::SampleRate::SAI_48KHZ;
    sai_cfg.bit_depth      = SaiHandle::Config::BitDepth::SAI_24BIT;
    sai_cfg.a_sync         = SaiHandle::Config::Sync::MASTER;
    sai_cfg.a_dir          = SaiHandle::Config::Direction::TRANSMIT;
    sai_cfg.pin_config.fs   = /* GPIO for LRCLK */;
    sai_cfg.pin_config.sck  = /* GPIO for BCLK */;
    sai_cfg.pin_config.sa   = /* GPIO for SDOUT */;
    sai_cfg.pin_config.mclk = /* MCLK pin or unused */;
    sai_out.Init(sai_cfg);

    hw.SetAudioBlockSize(48);
    hw.SetAudioSampleRate(SaiHandle::Config::SampleRate::SAI_48KHZ);
    hw.StartAudio(AudioCallback);
    sai_out.StartDma(/* TX buffer, size, callback */);

    while (1) {}
}
```

**Match these settings on both sides:**

- Sample rate: 48 kHz (Pi I²S supports 48 kHz universally; other rates
  may need clock-divider tweaks)
- Bit depth: 24-bit in 32-bit slots (Pi I²S works most reliably with
  S32_LE)
- Frame format: I²S (LRCLK low = left, one BCLK delay on first data bit)
- Channels: stereo (LRCLK toggles per sample)

If your sample rate / bit depth disagrees between Daisy and Pi, you'll
get either silence, noise, or one channel — symptoms vary.

---

## Raspberry Pi setup

### 1. Base OS

Raspberry Pi OS (64-bit, Bookworm or later). After first boot:

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y alsa-utils chromium-browser
```

### 2. Enable I²S in firmware config

Edit `/boot/firmware/config.txt` (on older Pi OS this lives at
`/boot/config.txt`). Add or uncomment:

```ini
dtparam=i2s=on
dtoverlay=googlevoicehat-soundcard
```

The `googlevoicehat-soundcard` overlay is the simplest path for generic
stereo I²S input on the standard pins. It was written for the Google
AIY Voice HAT but doesn't require that hardware to be present — it just
configures the Pi's I²S peripheral for stereo S32_LE capture at 48 kHz.

Reboot:

```bash
sudo reboot
```

### 3. Verify the card appears

```bash
arecord -l
```

You should see a card like `snd_rpi_googlevoicehat_soundcar` (note the
truncated name — that's normal). Note its card number, e.g. `card 1`.

### 4. Capture a test recording

With your Daisy running and outputting audio:

```bash
arecord -D plughw:1,0 -c 2 -r 48000 -f S32_LE -d 5 /tmp/test.wav
aplay /tmp/test.wav  # via 3.5mm jack to confirm
```

If `test.wav` has signal that matches what Daisy is outputting, the
I²S link is working. If it's silence: check clocks first (BCLK/LRCLK
must be present — a scope or logic analyzer will tell you in 10
seconds). If it's noise/distortion: you almost certainly have a
sample-rate or bit-depth mismatch.

---

## Browser integration

The visualizer uses `getUserMedia({ audio: true })` and routes the
stream through a `MediaStreamSource` → `AnalyserNode`. Chromium honors
the system default capture device, so:

### 1. Set the I²S card as default capture in ALSA

Create `~/.asoundrc`:

```
pcm.!default {
    type asym
    capture.pcm "hw:1,0"   # your card number from arecord -l
}
ctl.!default {
    type hw
    card 1
}
```

Or — preferred — configure PulseAudio / PipeWire (Bookworm uses
PipeWire by default) to select the I²S card. In the desktop, right-
click the volume icon → Input Devices → select the googlevoicehat
card. PipeWire persists this.

### 2. Launch the visualizer

```bash
chromium-browser --kiosk file:///path/to/ambient_viz/static/index.html?lite
```

The `?lite` flag is the 720p + sparse-lattice mode (see
`PI_PERFORMANCE.md`).

In the visualizer's UI, click **mic**. The browser will prompt for
microphone permission — grant it. If only one capture device is
visible (the I²S card), it'll be used automatically. If multiple are
visible (e.g. you have a USB camera with a mic), Chromium may need
explicit selection — use `chrome://settings/content/microphone` to
pick the I²S card as default.

---

## Verifying end-to-end

Quick sanity checklist:

1. `arecord -l` shows the googlevoicehat card. **If not:** the overlay
   didn't load — check `/boot/firmware/config.txt` and `dmesg | grep -i
   i2s` for errors.
2. `arecord -D plughw:1,0 -c 2 -r 48000 -f S32_LE -d 3 /tmp/t.wav`
   produces a non-silent file when Daisy is playing. **If silent:**
   scope BCLK first (should be ~3 MHz), then LRCLK (should be 48 kHz),
   then SDOUT (data toggling). Missing BCLK means Daisy SAI isn't
   running or isn't routed to the header pin you wired.
3. Chromium → visualizer → mic button → audio bands react. **If not
   reacting:** open the visualizer's dev panel (`~` key) and check
   that the live `bass`/`mid`/`treble` readouts move. If they're zero
   but `arecord` works, it's a browser-side device-selection issue.

---

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `arecord` returns silence | No clocks reaching Pi, or Pi expects master and Daisy is slave (both ends slave = no clocks). Scope BCLK to confirm. |
| `arecord` returns loud white noise | Sample-rate mismatch or bit-depth mismatch. Daisy at 48k/24, Pi reading at 44.1k or 16-bit, etc. |
| Audio is half-speed or double-speed | LRCLK rate mismatch. Most likely Daisy is running at a sample rate the Pi I²S clock divider can't match exactly. Stick to 48 kHz. |
| One channel silent | LRCLK polarity inverted (left/right swapped is OK; one channel missing means LRCLK isn't toggling correctly — usually a wiring or SAI-config error). |
| Constant `xrun` / underrun in `arecord` logs | Pi I²S clock domain isn't locking to Daisy's BCLK. Possible PSU brownout, marginal jumper-wire signal integrity, or Daisy BCLK is too far from a divisible Pi rate. |
| Visualizer mic button does nothing | Chromium needs explicit permission; check `chrome://settings/content/microphone`. Also: HTTPS or `file://` is required for `getUserMedia` — `http://` won't prompt. |
| Visualizer bands all zero but `arecord` shows audio | Wrong default capture device. `pactl list short sources` (PipeWire) and set the I²S card as default. |
| Pi reboots when Daisy is plugged in | Powering Daisy from the same Pi USB rail under load. Power Daisy from a separate USB supply. |

---

## Why not use the Pi's own audio in?

The Pi 4 has **no built-in audio input.** The 3.5mm jack is output-only
(on Pi 4 it's also AV out for composite video). There is no ADC on
the SoC. That's why I²S (or USB audio) is the only path.

---

## Easier alternatives (if the above feels like too much)

1. **Daisy as USB audio class device** (community firmware).
   The state of this as of mid-2026:
   - libDaisy itself **does not** implement USB Audio Class. The
     `src/hid/usb.h` header notes "currently only CDC is supported";
     USB MIDI is the only other USB device class wired up.
   - The ST middleware submodule (`Middlewares/ST/STM32_USB_Device_Library`,
     pulling from `STMicroelectronics/stm32_mw_usb_device`) ships a
     UAC1 reference (`Class/AUDIO/`), but the default direction is
     playback-only (`AUDIO_OUT_EP` defined; host → Daisy as a USB
     speaker). Wrong direction for our use case out of the box.
   - A community implementation by forum user **nadavb** ports the ST
     middleware to a stereo **input** UAC1 device. Posted as a ~16 KB
     `.zip` of C/H files to drop into a Daisy project. See:
     <https://forum.electro-smith.com/t/custom-usb-audio-uac-x2-input-interface/9047>
   - Enumerates natively as a stereo input on Linux/macOS/Windows
     (UAC1 has in-kernel support on all three). On the Pi: plug in,
     `arecord -l` shows it, Chromium picks it up. No DT overlays.
   - Daisy's USB is full-speed (12 Mbit/s) only — fine for
     48 kHz / 24-bit stereo (~2.3 Mbit/s payload).
   - **Known caveat:** the ST UAC reference has no resampling / drift-
     control loop, so there are audible glitches from clock drift
     between Daisy's audio clock and the host's USB SOF clock. Worse
     under heavy Daisy CPU load (reported at 85%+).
   - **For the visualizer this caveat is essentially moot.** Brief
     drift artifacts don't survive into the FFT-band envelopes that
     drive the visuals. If you ever wanted to use Daisy as a recording
     interface, drift matters — for driving lattice/sparks/dither, it
     doesn't.
   - Trade-off vs. I²S: you depend on third-party unmaintained
     firmware. Plus side: by far the lowest-effort path to a working
     audio source on the Pi — minutes once the firmware is flashed.

2. **Cheap USB audio interface.** A $15 Behringer UCA222 or similar
   takes Daisy's line-out RCAs into its line-in, presents as USB audio
   class to the Pi. Same zero-config story. You also get a clean ADC
   front-end — useful if you ever want to run the visualizer off other
   line-level sources.

3. **HiFiBerry DAC+ ADC HAT.** Drop-in HAT with stereo line-in, well-
   supported overlay (`dtoverlay=hifiberry-dacplusadc`). About $40 but
   it's basically plug-and-play and uses a proper ADC chip. Overkill
   if you only ever want Daisy as the source.

If your goal is "see the visualizer react to Daisy on a Pi" and not
"build an all-digital signal chain on principle":

- **Option 2 (USB audio interface)** is the lowest-friction path —
  literally plug-and-play on the Pi, no firmware work on either side.
  Best choice if you don't already have one and don't mind ~$15.
- **Option 1 (community UAC firmware)** is the cheapest path (zero new
  hardware), but you accept building/flashing third-party firmware
  and the clock-drift caveat (inaudible to the visualizer, but a
  real limitation if you ever want Daisy as a recording interface).
- **Option 3 (HiFiBerry HAT)** if you want something officially
  supported and don't mind ~$40.

The I²S route in this guide is worth doing if:
- You care about avoiding the analog round-trip (Daisy DAC → cable →
  USB-interface ADC).
- You want first-party-only firmware on the Daisy side (no community
  patches) and don't mind ST middleware-style DT overlay work on the
  Pi side.
- You want the experience of building it.
- You may extend the setup later with more I²S devices on the same
  bus.

For most people building this specifically to drive the visualizer:
Option 2 first, Option 1 if you want to avoid spending anything, this
I²S guide if you specifically want the all-digital path.
