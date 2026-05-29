# Day-of-Installation Setup

On-site checklist for installing an **already bench-tested** kiosk at the venue.
For first-time/bench bringup or any failure, fall back to `PI_KIOSK_BRINGUP.md`.

The one venue-specific step is calibrating the distance sensor to the
curator's projector — everything else should already work.

---

## 1. Mount

- Place VL53L1X at the screen plane, **facing outward** toward viewers (not at the screen).
- **Recess it ~1 cm behind the screen surface** (or add a small shroud) so the projector beam can't graze its cover glass.
- Sensor at adult chest height; cone is a narrow ~15°.
- Check the sensor's shadow on the projected image — hide it (bezel / bottom edge) or accept it.

## 2. Power on + bus check

```sh
cd ~/ambient_viz && sudo i2cdetect -y 1
```

- `0x29` (distance) and `0x5A` (touch) must show, steady across repeated scans.
- Missing/flickering → `PI_KIOSK_BRINGUP.md` Phase 3.

## 3. Calibrate distance sensor to the projector  ← the venue-specific step

```sh
cd ~/ambient_viz/python && source .venv/bin/activate
python test_vl53l1x.py
```

- Point at the real wall. Read the **`amb`** column with the **projector OFF**, then **ON**.
- Set `VL53_AMBIENT_LONG_MAX` in `python/ambient_kiosk/config.py` **just above the OFF (dark) baseline**.
  - Projector adds little IR (LED/laser) → leaves headroom → long mode, more reach.
  - Projector floods IR (bright lamp) → ON value exceeds the threshold → short mode, ambient-immune.
- Sanity-check accuracy: held target tracks within ~2 cm @ 30 cm, ~5 cm @ 100 cm.

## 4. Launch + confirm auto-mode

```sh
cd ~/ambient_viz && ./run_kiosk.sh
```

- Watch the sidecar log for the auto-select decision:
  ```
  distance: ambient median=… (long_max=…) -> short mode
  distance: VL53L1X ready (mode=1, budget=20ms)
  ```
- `mode=2` = long, `mode=1` = short. Confirm it matches what step 3's numbers implied.
- Wave a hand: `distance_cm` should track ~100 cm → ~5 cm and back.

## 5. Display

- Chromium kiosk URL:
  ```
  http://localhost:8080/?lite=1&bitmap=360&distanceToBitmap=on
  ```
- Walk toward the screen — the bitmap ceiling should tighten as you approach.
- For unattended autostart (systemd + Chromium): `PI_KIOSK_BRINGUP.md` Phase 9.

## 6. Before walking away

- Reboot once; confirm the kiosk comes back up unattended and the auto-mode log reappears.
- Re-run step 3 if the projector or its position changes during the run.

---

**Ping back if:** ambient reads near `long_max` in both modes (effect feels unreliable),
the sensor can't see past ~1 m even in long mode, or the projector shadow can't be hidden
acceptably. Capture the sidecar log either way.
