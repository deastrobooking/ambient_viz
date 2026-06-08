# Pi 4 Audio Test Deployment

Runbook for using a Raspberry Pi 4 as the test companion for the separate
audio-instrument fork.

The Pi is not the primary audio engine. The definitive audio runtime is the
Daisy/Rust engine and, during development, the macOS `daisy/crates/host`
harness. Use the Pi for deployment testing, visual sync, sensor input, USB CDC
telemetry/control, and kiosk validation around the instrument.

For the original all-sensors-to-browser exhibit bringup, use
`PI_KIOSK_BRINGUP.md`. This guide is the audio-fork path.

## Test Modes

| Mode | Use When | Runs On Pi |
| --- | --- | --- |
| Mock companion | You want to confirm Node, Chromium, SSE, and visual sync without hardware. | Node server, optional Chromium. |
| Sensor companion | You want Pi sensors publishing events for visual sync or future control mapping. | Node server, Python sidecar, optional Chromium. |
| Daisy companion | You want Pi to listen to Daisy CDC song position and forward sensor/freeze CCs to Daisy. | Node server with `DAISY_SERIAL`, optional Python sidecar and Chromium. |

Keep audio acceptance focused on Daisy codec/line out. USB audio capture on the
Pi is diagnostic only and should not block groovebox milestones.

## Bench Checklist

- Raspberry Pi 4 Model B, 4 GB or better preferred.
- Raspberry Pi OS Bookworm 64-bit.
- Official 5.1 V / 3 A USB-C power supply.
- Network access or direct keyboard/display.
- This repository cloned on the Pi, usually `~/ambient_viz`.
- Daisy Seed connected by USB when testing CDC/UAC.
- Daisy line out connected to speakers, mixer, or headphones through the normal
  analog path.
- Optional: HDMI display for Chromium visual sync.
- Optional: sensors from `hardware-handoff.md`.

## Phase 0: OS Prep

```sh
sudo apt update
sudo apt install -y \
    git curl build-essential pkg-config \
    nodejs npm \
    python3 python3-venv python3-pip \
    chromium-browser \
    i2c-tools pigpio

sudo raspi-config nonint do_i2c 0
sudo systemctl enable --now pigpiod
sudo reboot
```

Verify after reboot:

```sh
node --version
npm --version
python3 --version
ls /dev/i2c-1
systemctl is-active pigpiod
```

Notes:

- `pigpiod` and I2C are needed only for sensor companion testing.
- If `chromium-browser` is unavailable on the OS image, install the distro's
  Chromium package and keep the rest of the guide unchanged.

## Phase 1: Repo Setup

```sh
cd ~
git clone <repo-url> ambient_viz
cd ambient_viz
```

If the repo is already present:

```sh
cd ~/ambient_viz
git status --short
```

Do not run broad cleanup commands on the Pi test checkout. Keep deployment
changes explicit and copy them back through normal source control.

## Phase 2: Node Bridge

Install the bridge dependencies. The current bridge uses `serialport` for Daisy
CDC, so run `npm install` even if you are only starting with mock SSE.

```sh
cd ~/ambient_viz/server
npm install
```

Start with no Daisy and no Python sidecar:

```sh
MOCK=1 npm start
```

Verify from another terminal:

```sh
curl -s http://localhost:8080/ | head
curl -s -N http://localhost:8080/events
```

Expected:

- HTTP serves the visualizer.
- `/events` emits SSE frames.
- Mock values update without sensor hardware.

Open Chromium:

```sh
chromium-browser http://localhost:8080/?debug=1
```

If the page renders and `window.AMBIENT_INPUTS.__meta.connected` is true in the
browser console, the Pi companion layer is alive.

## Phase 3: Python Sensor Sidecar

Skip this phase if you are testing Daisy CDC only.

```sh
cd ~/ambient_viz/python
python3 -m venv .venv
source .venv/bin/activate
pip install -e .
```

Mock sensor run:

```sh
cd ~/ambient_viz/server
npm start
```

```sh
cd ~/ambient_viz/python
source .venv/bin/activate
python -m ambient_kiosk --mock
```

Verify events:

```sh
curl -s -N http://localhost:8080/events | grep -E 'distance_cm|touch_mask|motion|breath'
```

Hardware sensor run:

```sh
cd ~/ambient_viz/python
source .venv/bin/activate
python -m ambient_kiosk
```

Use `PI_KIOSK_BRINGUP.md` if any sensor fails to appear. This guide assumes
sensors are already electrically valid.

## Phase 4: Daisy USB/CDC Companion

Connect Daisy USB to the Pi and confirm Linux sees the composite device:

```sh
dmesg --follow
```

In another terminal after plugging Daisy in:

```sh
ls /dev/ttyACM*
```

Expected default serial path on Pi:

```text
/dev/ttyACM0
```

Start the Node bridge with Daisy CDC enabled:

```sh
cd ~/ambient_viz/server
DAISY_SERIAL=/dev/ttyACM0 npm start
```

Verify song-position telemetry:

```sh
curl -s -N http://localhost:8080/events | grep song_position
```

Expected:

- `song_position` changes roughly 20 times per second while the firmware emits
  `POS`/`RESET`.
- Browser visual sync can use `?clock=daisy` once this stream is stable.
- Do not open `/dev/ttyACM0` in another terminal while Node is running; the
  bridge is the single serial owner.

Useful browser URL:

```text
http://localhost:8080/?clock=daisy&debug=1
```

## Phase 5: Sensor/Frozen-State Control To Daisy

The current bridge can forward selected browser/sensor changes to Daisy as MIDI
bytes over CDC:

- `distance_cm` maps to CC 23 in the legacy kiosk binding.
- browser `freeze` posts map to CC 24.
- entry/toll/voice triggers can send MIDI note-on messages when enabled in the
  server path.

This is legacy-compatible control plumbing. For the rebuilt groovebox, prefer
new control surfaces that translate into `GrooveEvent` first. Until the firmware
accepts the full `GrooveEvent` vocabulary, treat the CDC MIDI path as a bridge
for install tests, not the final product protocol.

Verification:

```sh
curl -s -N http://localhost:8080/events | grep -E 'distance_cm|song_position'
```

Then move in front of the ToF sensor or use `python -m ambient_kiosk --mock`.
Listen on Daisy line out for the mapped effect response.

## Optional: USB Audio Capture Diagnostic

USB audio capture from Daisy to Pi is useful for diagnostics, not for product
acceptance. Prefer analog Daisy line out for listening tests.

When diagnosing UAC:

```sh
arecord -l
```

If Daisy appears as a capture device, run a short capture and watch firmware
diagnostic counters over debug UART where available:

```sh
arecord -D <daisy-device> -f S16_LE -r 48000 -c 2 -d 10 /tmp/daisy-test.wav
```

Interpretation:

- audio OK on line out but capture unstable: keep product work moving, file a
  USB/Pi diagnostic task.
- `usb_drop` increasing: firmware/USB ring or SD stalls may be involved.
- `usb_pktmax` pinned near packet cap with low drops: host polling/capture
  pacing may be involved.

## Service Sketch

Use manual terminals while iterating. Once a test setup is stable, create user
services.

Node companion service:

```ini
[Unit]
Description=ambient_viz Node companion
After=network-online.target

[Service]
WorkingDirectory=/home/pi/ambient_viz/server
Environment=DAISY_SERIAL=/dev/ttyACM0
ExecStart=/usr/bin/npm start
Restart=on-failure
User=pi

[Install]
WantedBy=default.target
```

Python sidecar service:

```ini
[Unit]
Description=ambient_viz sensor sidecar
After=network-online.target ambient-viz-node.service

[Service]
WorkingDirectory=/home/pi/ambient_viz/python
ExecStart=/home/pi/ambient_viz/python/.venv/bin/python -m ambient_kiosk
Restart=on-failure
User=pi

[Install]
WantedBy=default.target
```

Keep service names local to the install until the deployment story settles.

## Quick Fault Map

| Symptom | First Check |
| --- | --- |
| Browser does not connect to SSE | `curl -s -N http://localhost:8080/events`; confirm Node process. |
| Node cannot open Daisy | `ls /dev/ttyACM*`; unplug/replug Daisy; stop any other serial monitor. |
| No `song_position` | firmware CDC task not emitting, wrong `DAISY_SERIAL`, or Daisy not configured. |
| Sensor events absent | run `python -m ambient_kiosk --mock`; if mock works, use `PI_KIOSK_BRINGUP.md`. |
| Audio heard on line out but not Pi capture | treat as USB diagnostic, not a groovebox blocker. |
| Visualizer heavy on Pi 4 | use `PI_PERFORMANCE.md`; do not optimize browser visuals before audio milestones. |

## Acceptance

A Pi 4 test deployment is healthy when:

- Node starts with `npm start`.
- `/events` streams mock or live input.
- Chromium can load the visualizer locally.
- With Daisy attached, `song_position` streams over SSE from CDC.
- Daisy audio is judged from analog codec/line out.
- Any sensor-to-audio experiment is documented as either legacy MIDI-CC bridge
  behavior or a future `GrooveEvent` control surface.
