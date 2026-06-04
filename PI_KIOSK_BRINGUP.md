# Pi Kiosk Bringup

End-to-end runbook: bare Pi 4 → all four sensors streaming into the
visualizer. Every phase has a verification before the next one
starts — diagnose immediately rather than chasing compound failures.

**Authoritative hardware spec is `hardware-handoff.md`.** This doc is
the *sequence*; the handoff doc is the *what*. If they disagree, the
handoff doc wins.

The work assumes you've cloned this repo onto the Pi and can SSH or
sit at it directly.

---

## Phase 0 — What you need on the bench

- Pi 4 Model B (or 5), Raspberry Pi OS Bookworm or newer
- Official 5.1V/3A USB-C PSU (underpowered PSUs cause I²C dropouts;
  the handoff doc flags this)
- microSD with Pi OS, networked
- All sensors per the handoff BOM, TLC555 oscillator built and
  verified-oscillating (multimeter or scope) before connecting to GPIO
- HDMI display + cable for the visualizer
- 4× I²C pull-ups (4.7 kΩ each, two used) and decoupling caps
- A way to view the visualizer: either Chromium on the Pi or any
  browser on the same LAN

---

## Phase 1 — OS prep

```sh
sudo apt update
sudo apt install -y \
    i2c-tools \
    pigpio \
    python3 python3-venv python3-pip \
    nodejs npm \
    chromium-browser

# Enable I²C (kernel module + /dev/i2c-1)
sudo raspi-config nonint do_i2c 0
# If using the VL53L5CX: raise the bus to 400 kHz so its ~84 KB boot firmware
# upload isn't painfully slow. Add to /boot/firmware/config.txt:
#   dtparam=i2c_arm=on,i2c_arm_baudrate=400000
# 400 kHz is the shared-bus max (MPR121 + ADS1115 limit) — do NOT use 1 MHz
# with the MPR121 present. Skip this if you're only running the VL53L1X.

# pigpiod is needed only by the breath driver, but enable it now
sudo systemctl enable --now pigpiod

sudo reboot
```

**Verify after reboot:**

```sh
node --version             # ≥ 18
python3 --version          # ≥ 3.9
ls /dev/i2c-1              # exists
systemctl is-active pigpiod  # active
```

If any of these fail, fix here before moving on.

---

## Phase 2 — Software stack with no hardware

Validate Node + Python + visualizer before any GPIO variables enter
the picture. This phase needs zero sensors wired.

```sh
cd ~/ambient_viz

# Node bridge — runs in foreground, leave open
cd server
node src/index.js
```

In another terminal:

```sh
cd ~/ambient_viz/python
python3 -m venv .venv
source .venv/bin/activate

# Editable install. On a Pi this also installs gpiozero, pigpio,
# adafruit-blinka, adafruit-circuitpython-vl53l1x, and
# adafruit-circuitpython-mpr121 (they're marked Pi-only in
# pyproject.toml).
pip install -e .

python -m ambient_kiosk --mock
```

Open `http://<pi-ip>:8080/` in any browser. You should see the
visualizer running. In the browser dev console:

```js
window.AMBIENT_INPUTS
// → {distance_cm: 32.4, touch_mask: 0, breath_detected: 1747800000000}
window.AMBIENT_INPUTS.__meta.connected   // true
```

If `distance_cm` is changing once per second-ish, `breath_detected`
bumps every ~14 s, and `touch_mask` drifts, the software pipeline is
sound. **Failures here are not hardware bugs.**

`Ctrl-C` both processes when satisfied.

---

## Phase 3 — I²C bus integrity

Wire **only the three I²C devices** plus the pull-ups. Leave PIR,
TLC555, and MPR121 IRQ disconnected for now. Reasoning: I²C bus
integrity is its own failure domain — isolate it before introducing
edge inputs.

Per `hardware-handoff.md` §I²C Bus, §Pin Assignments, §VL53L1X §MPR121
§ADS1115:

- 4.7 kΩ resistor: SDA rail → 3.3V rail
- 4.7 kΩ resistor: SCL rail → 3.3V rail
- VL53L1X, MPR121, ADS1115 all on the shared SDA/SCL
- Jumpers < 15 cm; star-ground all sensor GNDs back to one Pi GND pin

**Verify:**

```sh
sudo i2cdetect -y 1
```

Expected:

```
     0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
00:                         -- -- -- -- -- -- -- --
10: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --
20: -- -- -- -- -- -- -- -- -- 29 -- -- -- -- -- --
30: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --
40: -- -- -- -- -- -- -- -- 48 -- -- -- -- -- -- --
50: -- -- -- -- -- -- -- -- -- -- 5a -- -- -- -- --
60: -- -- -- -- -- -- -- -- -- -- -- -- -- -- -- --
70: -- -- -- -- -- -- -- --
```

If `0x29` / `0x48` / `0x5A` is missing or flickers between repeated
scans, **stop and debug the bus** before going further:

- Re-check the pull-ups (multimeter: ~4.7 kΩ from SDA to 3.3V rail
  with power off)
- Verify 3.3V at each device's Vcc pin with power on
- Verify GND continuity at each device
- Try a slower bus speed: add `dtparam=i2c_arm_baudrate=50000` to
  `/boot/firmware/config.txt`, reboot, retest
- Shorten jumpers; twist SDA-GND and SCL-GND together if you have
  long runs

(`0x48` is the ADS1115 — it's on the bus but the software doesn't use
it yet. If you didn't wire the ADS1115, only `0x29` and `0x5A` should
appear; that's also fine.)

---

## Phase 4 — Distance sensor alone (VL53L1X)

**Standalone hardware check first** — bypass the kiosk stack to confirm
the sensor reads accurately before involving the SSE pipeline:

```sh
cd ~/ambient_viz/python && source .venv/bin/activate
python test_tof.py          # auto-detects L1X vs L5CX; force with: python test_tof.py l1x
```

Prints live readings + rolling 1 s mean/stddev. Hold a target steady
at known distances (tape measure) and confirm the mean tracks within
~2 cm at 30 cm, ~5 cm at 100 cm. If this script reports accurate
values but the SSE feed (later) reads differently, the divergence is
in the sidecar smoothing — see `SENSOR_MAPPING.md`.

**Two-process launch** — Node bridge in one terminal, sidecar in
another:

```sh
cd ~/ambient_viz/server && node src/index.js
```

```sh
cd ~/ambient_viz/python && source .venv/bin/activate
python -m ambient_kiosk --no-pir --no-breath --no-touch
```

Or use the convenience launcher from the repo root:

```sh
./run_kiosk.sh
```

`run_kiosk.sh` starts both, interleaves their logs with `[node]` /
`[py  ]` prefixes, propagates Ctrl-C, and echoes the recommended
browser URL (with `?distanceToBitmap=on`) at startup. Default sidecar
args have `--no-pir --no-breath`; pass overrides as positional args.

You should see in the sidecar log:

```
distance: VL53L1X ready (mode=1, budget=20ms)
```

In a third terminal, watch the live SSE stream:

```sh
curl -s -N http://localhost:8080/events | grep distance_cm
```

Wave your hand in front of the sensor. You should see `distance_cm`
values track the hand from ~100 cm down to ~5 cm and back. The values
will be smoothed (α=0.25) so motion lags by ~80 ms.

**If the driver logs `VL53L1X init failed`** — re-run `i2cdetect`.
0x29 must be visible. If the bus shows it but init fails, the cheap
clones sometimes need a slower bus; try the 50 kHz setting in
phase 3's troubleshooting.

**Mount it** for real use only after this works. Aim at adult chest
height per the handoff doc; the 4×4 SPAD ROI is a narrow ~15° cone.

`Ctrl-C` the sidecar.

---

## Phase 5 — Cap touch alone (MPR121)

Wire the MPR121 IRQ pin to GPIO27 (BCM). Attach a few electrodes (foil
pads, conductive thread, whatever the kiosk uses) to channels E0–E11.

```sh
python -m ambient_kiosk --no-pir --no-distance --no-breath
```

Log:

```
touch: MPR121 + IRQ on BCM27
```

Watch the bitmask:

```sh
curl -s -N http://localhost:8080/events | grep touch_mask
```

Touch each electrode. The mask should show the corresponding bit set:
E0 → 1, E1 → 2, E2 → 4, ..., E11 → 2048. Multi-touch sums the bits.

**The MPR121 auto-calibrates** its baseline for ~30 s after init. Touch
behavior may be erratic during that window — that's expected per the
handoff doc, not a wiring bug.

If a specific channel is over- or under-sensitive after the cal
settles, per-channel thresholds can be tuned in `sensors/touch.py`
via the Adafruit library's `_set_thresholds()` (not currently exposed
in `config.py` — add it there when you know the numbers).

**Visualizer effect:** Open `http://<pi-ip>:8080/` in a browser
(start the Node bridge in another terminal first). Each electrode
fades a distinct color into the screen's color overlay when touched
(E0=red, E1=orange, E2=yellow, E3=lime, E4=green, E5=teal, E6=cyan,
E7=blue, E8=purple, E9=magenta, E10=pink, E11=white). Default rise
τ = 8 s, fall τ = 18 s — first noticeable shift takes a few seconds
of hold; full settling takes 15-25 s. Multiple simultaneous touches
blend by weighted average. Live-tunable from the dev panel via the
`touchRiseS` / `touchFallS` sliders.

If `touch_mask` is updating in the SSE stream but the tint isn't
shifting, check the DevTools console for `window.AMBIENT_INPUTS.touch_mask`
— if it's stuck at 0, the browser isn't seeing SSE events (firewall,
wrong host, opened via `file://`). The mask flows main → worker on
every rAF in the `audio` postMessage; no separate channel.

Tuning: rise/fall time constants are dev-panel sliders (and
timeline-automatable). Per-electrode colors are still inline in the
worker source (`TOUCH_COLORS` in `static/index.html`) — edit if you
need to recolor.

---

## Phase 6 — PIR alone (AM312)

Wire AM312 VCC/GND/OUT to 3.3V / GND / GPIO4. Use the included Fresnel
lens.

```sh
python -m ambient_kiosk --no-distance --no-breath --no-touch
```

Log:

```
pir: AM312 on BCM4 (suppressing 60s post-boot)
```

**Wait the full 60 seconds.** During this window the sidecar
deliberately drops PIR events to mask the AM312's settling phase
(unstable output during the first 30–60 s after power-up). The log
will look silent — that's correct.

After 60 s, watch motion:

```sh
curl -s -N http://localhost:8080/events | grep motion
```

Wave laterally past the sensor (the AM312 is blind to motion *toward*
it). You should see `motion: true` on entry, then `motion: false`
~2 s after motion stops (AM312's fixed hold time).

If the sensor stays stuck at `true`, the Fresnel lens may be missing
or the sensor may be sitting in a draft from HVAC. Reposition.

---

## Phase 7 — Breath alone (HR202 + TLC555)

This is the most involved phase. **Verify the TLC555 is oscillating
before connecting to the Pi GPIO**, otherwise you're debugging two
problems at once.

### 7a. Verify TLC555 oscillation

Build the 555 circuit per `hardware-handoff.md` §HR202. With Vcc
powered but pin 3 **not** yet connected to the Pi:

- If you have a scope or USB logic analyzer: probe pin 3. In normal
  room air the output should be a square wave around **1–2 kHz**
  (theoretical formula in the handoff doc gives ~1.5 kHz at 100 kΩ).
- If you only have a multimeter: pin 3's DC average should sit around
  half of Vcc (~1.65 V) — neither stuck high nor stuck low.

**If the 555 isn't oscillating:**
- Did you use a **TLC555CP** (CMOS)? A bipolar NE555 won't drive
  R2 in the MΩ range. Handoff doc §Known Gotchas.
- Is the timing cap C0G? Y5V/Z5U bulk ceramics drift wildly with
  temperature and load.
- Check pin 4 (RESET) is tied high to Vcc, not floating.

Once you have oscillation, connect pin 3 to Pi GPIO17.

### 7b. Verify edge counting in pigpio

Before running the full driver, sanity-check that pigpio sees the
edges:

```sh
pigs r 17    # read current pin level — should toggle between scans
```

A more thorough check (counts edges over 1 s):

```sh
python3 -c "
import pigpio, time
pi = pigpio.pi()
n = [0]
cb = pi.callback(17, pigpio.RISING_EDGE, lambda g,l,t: n.__setitem__(0, n[0]+1))
time.sleep(1.0)
cb.cancel()
print('edges/s:', n[0])
pi.stop()
"
```

Expected: ~1500 ± 500 in normal room air. If you get 0, GPIO17 isn't
seeing the 555's output — check wiring and that pigpiod is running.
If you get something wildly off (e.g. 50 Hz or 60 Hz), you're picking
up mains hum; check that the 555 ground is solidly connected to the
Pi ground.

### 7c. Run the breath driver

```sh
python -m ambient_kiosk --no-pir --no-distance --no-touch
```

The first **10 seconds** are a warmup window during which the driver
seeds its baseline — no detection events fire. After warmup:

```sh
curl -s -N http://localhost:8080/events | grep breath_detected
```

Blow gently on the HR202 from 5–10 cm. The driver should log:

```
breath: detected (freq=4500 baseline=1500)
```

and `breath_detected` should appear in the SSE stream. Then a 3 s
debounce kicks in — subsequent immediate breaths won't fire (correct).

**Tuning** (in `python/ambient_kiosk/config.py`):

- `BREATH_TRIGGER_RATIO` (1.3): raise if false-positives, lower if
  breaths aren't firing
- `BREATH_BASELINE_ALPHA` (0.02): slower (smaller) baseline means
  longer puffs still register; faster means baseline tracks slow
  ambient changes better but absorbs sustained breaths
- `BREATH_DEBOUNCE_S` (3.0): minimum gap between two breath events

Empirically tune with the actual kiosk placement (sensor-to-mouth
distance, ambient airflow) before locking in.

---

## Phase 8 — All four sensors together

Once each sensor passed its solo run:

```sh
python -m ambient_kiosk
```

Expected log:

```
pir: AM312 on BCM4 (suppressing 60s post-boot)
distance: VL53L1X ready (mode=1, budget=20ms)
breath: TLC555 edge counter on BCM17
touch: MPR121 + IRQ on BCM27
running (hardware; 4 drivers; ingest -> http://127.0.0.1:8080/ingest)
```

Open the visualizer in Chromium on the Pi. The dev console should
show `window.AMBIENT_INPUTS` populating across all four names as you
interact. The visualizer itself doesn't bind to these yet — that's the
next step (mapping sensor values to viz parameters).

If anything regresses when you turn everything on that worked solo,
it's almost always one of:
- I²C contention (two libraries trying to grab the bus at once —
  shouldn't happen with our adafruit-blinka usage, but a thread race
  is possible)
- pigpiod conflict (gpiozero default backend can collide with pigpio
  in some configurations — if PIR and breath together misbehave but
  individually work, switch gpiozero to the lgpio backend:
  `export GPIOZERO_PIN_FACTORY=lgpio` before launching)

---

## Phase 9 — Autostart (systemd)

For an unattended kiosk, install two systemd units. Use **user**
services (not system services) so they share the desktop session and
GUI access for Chromium.

`~/.config/systemd/user/ambient-viz-server.service`:

```ini
[Unit]
Description=ambient_viz Node SSE bridge
After=graphical-session.target

[Service]
WorkingDirectory=%h/ambient_viz/server
# DAISY=1 loads the Daisy USB-CDC song-position bridge (Phase D) so the
# visualizer can run with ?clock=daisy. Without it, no `song_position` is
# published and clock=daisy freezes the lanes. systemd does NOT run ExecStart
# through a shell, so this must be an Environment= line — prepending DAISY=1 to
# ExecStart would make systemd look for a binary named "DAISY=1". The service
# user must also be in the `dialout` group to open /dev/ttyACM0.
Environment=DAISY=1
ExecStart=/usr/bin/node src/index.js
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

`~/.config/systemd/user/ambient-kiosk-sensors.service`:

```ini
[Unit]
Description=ambient_kiosk Python sensor sidecar
After=ambient-viz-server.service
Requires=ambient-viz-server.service

[Service]
WorkingDirectory=%h/ambient_viz/python
ExecStart=%h/ambient_viz/python/.venv/bin/python -m ambient_kiosk
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Enable:

```sh
systemctl --user daemon-reload
systemctl --user enable --now ambient-viz-server.service ambient-kiosk-sensors.service
systemctl --user status ambient-viz-server.service ambient-kiosk-sensors.service
loginctl enable-linger $USER   # keeps services running across logout
```

For Chromium kiosk autostart, add to `~/.config/autostart/kiosk.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=ambient_viz kiosk
Exec=chromium-browser --kiosk --noerrdialogs --disable-infobars --autoplay-policy=no-user-gesture-required "http://localhost:8080/?lite=1&bitmap=360&distanceToBitmap=on"
X-GNOME-Autostart-enabled=true
```

**The URL must be double-quoted.** The `&` separating query params is a
reserved character in a `.desktop` `Exec=` line — unquoted, the launcher
splits the command at each `&` and Chromium receives only `?lite=1`,
silently dropping `bitmap=360` and `distanceToBitmap=on`. That renders at
full native resolution and saturates the Pi 4's GPU, which freezes the
cursor/desktop (the page itself still loads, so it looks like it "works").
Verify with `pgrep -a chromium-browser` — the URL should be intact, not cut
off at the first `&`.

`?lite=1` hides DOM overlays and sparsens the lattice.
`?bitmap=360` caps render bitmap height to 360px (browser upscales to
fill the display). The Pi 4's V3D GPU is bandwidth-bound on the
per-frame WebGL texture uploads (dither + twist passes); rendering at
360p cuts that bandwidth ~4× vs 720p and is what gets us to acceptable
fps. Aesthetic cost: chunky dither / CRT-y look, which fits the
visualizer's vibe. Try `bitmap=480` if you want a sharper look at the
cost of fps, or drop both flags entirely if your Pi handles native res.
`?distanceToBitmap=on` wires the VL53L1X reading to scale the bitmap
ceiling down to 64 px as someone walks away (full res within 75 cm,
lowest res at/beyond the sensor's reach — ~130 cm short mode / ~400 cm
long mode) — see `SENSOR_MAPPING.md`
for the full mapping. Append `&debug=1` to surface a diagnostic
overlay that survives lite mode.

### Hiding the mouse cursor (Wayland)

The kiosk runs under Wayland, where the usual X11 cursor-hiding tricks
(`unclutter`, `xdotool`, etc.) don't work — they target the X server,
which isn't there. Instead, hide the pointer at the evdev layer with
[interception-tools](https://gitlab.com/interception/linux/tools) and its
`hideaway` plugin, driven by `udevmon`. This is compositor-agnostic: it
intercepts the pointer device before the cursor ever reaches the
compositor.

```bash
sudo apt install interception-tools          # provides udevmon + intercept/uinput
# install the hideaway plugin (interception-tools-hideaway), then point
# udevmon at it via /etc/interception/udevmon.d/*.yaml and enable the service:
sudo systemctl enable --now udevmon
```

If the cursor reappears, check that the `udevmon` service is running
(`systemctl status udevmon`) and that its config still routes the pointer
device through `hideaway`.

**Caveat — hideaway only covers a real mouse.** There are *three*
independent cursor sources, and hideaway only handles one:

1. **A physical evdev pointer** (USB mouse) → hideaway intercepts it. With
   no mouse plugged in there is no device to intercept, so hideaway does
   nothing here.
2. **The page's own cursor** → `static/index.html` already sets
   `body.lite-kiosk * { cursor: none !important; }` (active under
   `?lite=1`). But CSS `cursor: none` only applies *once a pointer enters
   the surface* — i.e. after a pointer device moves over the page. With no
   mouse, that pointer-enter event never fires, so the rule never gets a
   chance to run.
3. **The compositor's default cursor** → with no mouse connected, the
   Wayland compositor still parks its default pointer image on screen, and
   neither hideaway (no device) nor the page CSS (no pointer-enter) touches
   it. This is the stray static cursor you see over the kiosk; it only
   disappears when Chromium quits and the compositor's idle-hide kicks in.

**Fix for case 3 — a transparent cursor theme.** This kiosk runs **labwc**
(the recent Raspberry Pi OS default), which has no built-in cursor-hide, so
make the compositor's default cursor image itself invisible. Both labwc and
Chromium-on-Wayland (Ozone) honor the same `XCURSOR_THEME`, so one blank
theme covers both. Build it:

```bash
sudo apt install x11-apps imagemagick      # xcursorgen + convert
mkdir -p ~/.icons/blank/cursors
convert -size 1x1 xc:none /tmp/blank.png
printf '1 0 0 /tmp/blank.png\n' > /tmp/blank.cursor
xcursorgen /tmp/blank.cursor ~/.icons/blank/cursors/left_ptr
cd ~/.icons/blank/cursors
for n in default arrow top_left_arrow hand1 hand2 xterm crosshair watch left_ptr_watch; do ln -sf left_ptr "$n"; done
printf '[Icon Theme]\nName=blank\n' > ~/.icons/blank/index.theme
```

Then point labwc at it via its environment file
`~/.config/labwc/environment` (create the dir if needed):

```
XCURSOR_THEME=blank
```

Reload labwc (log out/in, or `labwc --reconfigure` for config — but the
env var needs a fresh session). Chromium relaunches under the kiosk
autostart and inherits the same theme.

(Steps written from the diagnosis above; not yet validated on the kiosk
hardware — confirm during bring-up.)

---

## Troubleshooting quick reference

| Symptom | Likely cause | Where to look |
|---|---|---|
| `i2cdetect` shows nothing | I²C not enabled, or no pull-ups | Phase 1, Phase 3 |
| One I²C address flickers | Weak/missing pull-up, long wires | Phase 3 |
| VL53L1X init fails despite 0x29 visible | Bus speed too high for clone | `dtparam=i2c_arm_baudrate=50000` |
| `breath:` driver logs `pigpio daemon not reachable` | `pigpiod` not running | `sudo systemctl start pigpiod` |
| Edge count is 0 from pigpio probe | TLC555 not oscillating, or pin 3 not on GPIO17 | Phase 7a |
| Edge count ~50 or ~60 Hz | Mains hum on a floating input | Check ground integrity to 555 |
| PIR stuck `true` forever | Missing Fresnel lens, HVAC draft, faulty AM312 | Reposition; swap module |
| MPR121 erratic for 30 s | Auto-calibration window | Wait it out |
| `window.AMBIENT_INPUTS` empty in browser | Bridge unreachable, or you opened `file://` | Visit `http://<pi-ip>:8080/`, not the file |
| SSE works locally, browser shows nothing | Different host/origin; firewall | Open the Pi's port 8080 to your LAN |

---

## When to ping back

Anything in this list is worth pinging back with — the runbook can't
diagnose them blind:

- Sensor passes solo but fails in Phase 8 → race condition, need to
  reproduce
- TLC555 doesn't oscillate even after the gotcha checks → schematic
  walk-through
- Breath detection is too jittery / too dead even after tuning →
  rethink baseline algorithm (one-euro filter, longer windows, etc.)
- Decisions about which visualizer parameter each sensor should drive
  (mapping `distance_cm` → which viz knob, etc.)

For each, capture the sidecar log (it'll be visible if you ran it in
the foreground; `journalctl --user -u ambient-kiosk-sensors -n 200`
if running under systemd).
