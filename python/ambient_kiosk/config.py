"""Pin assignments + tuning constants — authoritative copy of hardware-handoff.md."""

import os

# BCM pin numbers
# AM312 PIR OUT lines. Two units are fanned outward from the wall for wide room
# coverage and OR'd into a single `motion` channel (see hardware-handoff.md).
# Env-overridable as a comma list so a partial install (one AM312, or none) is a
# config change, not a code edit — e.g. PIR_PINS=4 for a single sensor, or
# PIR_PINS= (empty) to skip them entirely:
#   PIR_PINS=4 ./run_kiosk.sh
PIR_PINS = [int(p) for p in os.environ.get("PIR_PINS", "4,23").split(",") if p.strip()]
PIR_PIN = PIR_PINS[0] if PIR_PINS else 4  # back-compat: first sensor
BREATH_PIN = 17     # TLC555 pin 3 (OUTPUT) — frequency counter input
TOUCH_IRQ_PIN = 27  # MPR121 IRQ

# I2C addresses (shared bus on GPIO2/GPIO3)
VL53L1X_ADDR = 0x29
ADS1115_ADDR = 0x48  # unused, reserved
MPR121_ADDR = 0x5A

# AM312
PIR_BOOT_SUPPRESS_S = 60.0  # ignore output for the first 60s post-process-start

# ToF distance sensor selection. The VL53L1X and VL53L5CX both default to I²C
# address 0x29 but report distinct model IDs, so "auto" can tell them apart:
# it probes the L1X first (a cheap, non-destructive model-ID read) and only
# falls through to the L5CX (which uploads an ~84 KB firmware blob) when the
# L1X isn't wired. Pin it explicitly for a deterministic install boot.
VL53_SENSOR = "auto"       # "auto" | "l1x" | "l5cx"

# --- VL53L1X (single-point) -------------------------------------------------
VL53_DISTANCE_MODE = 1     # 1 = short, 2 = long. Fallback when auto-select
                           # is off or can't read ambient (see below).
# Timing budget (ms), per the VL53L1X datasheet: 20 ms is the floor and is valid
# ONLY in short mode; 33 ms is the floor for any mode; 140 ms is required to
# reach the full 4 m in long mode (dark, white target). The Adafruit lib accepts
# a discrete set {15,20,33,50,100,200,500}, so long mode uses 200 ms (the next
# value ≥140) to guarantee the 4 m reach — at the cost of a ~5 Hz ranging rate,
# which the hold/snap logic and browser EMA absorb. Mode-dependent so short mode
# keeps its fast 50 Hz feel. VL53_TIMING_BUDGET_MS is the short/default used for
# the boot ambient sample (always taken in short mode).
VL53_TIMING_BUDGET_MS_SHORT = 20
VL53_TIMING_BUDGET_MS_LONG = 200
VL53_TIMING_BUDGET_MS = VL53_TIMING_BUDGET_MS_SHORT

# Auto distance-mode select. The projector at the install is curator-supplied
# and unknown: a bright lamp engine throws real 940 nm IR onto the wall the
# sensor faces, which raises the SPAD noise floor and wrecks long-mode reach.
# At boot we sample the sensor's ambient IR rate and pick long mode only when
# the scene is dark enough to support it; otherwise short mode, which is far
# more ambient-tolerant. VL53_AMBIENT_LONG_MAX is in ST ULD units and MUST be
# tuned on-site: run test_tof.py with the projector ON, on the real wall,
# read the printed ambient rate, and set this just above the dark baseline.
VL53_AUTO_MODE = True
VL53_AMBIENT_CAL_S = 1.0       # how long to sample ambient before deciding
VL53_AMBIENT_LONG_MAX = 1500   # ambient at/below this -> long; above -> short

VL53_PUBLISH_HZ = 50
VL53_SMOOTH_ALPHA = 0.25
# "Far" reach (cm) — the no-target snap value AND the saturation end of every
# distance→effect mapping (twist, bitmap, tape failure), published downstream as
# `distance_far_cm`. Mode dependent: short mode reaches ~130 cm, long mode ~4 m.
# VL53_FAR_CM is the short/default used before mode is decided and in mock mode.
VL53_FAR_CM_SHORT = 130.0
VL53_FAR_CM_LONG = 400.0
VL53_FAR_CM = VL53_FAR_CM_SHORT
VL53_NEAR_CM = 25.0

# Seconds of continuous no-target (None) reads before the smoothed distance
# snaps to the far reach (reads "empty"). Longer rides out the multi-frame
# dropouts that flicker a present visitor to "empty" — dark clothing, an oblique
# torso, projector IR — at the cost of a genuine walk-away taking this long to
# register as idle. Presence detection favours ride-out over snappiness, so the
# default sits above the old 0.6 s. Env-overridable for install-day tuning, e.g.
#   NO_TARGET_TIMEOUT_S=2.0 ./run_kiosk.sh
NO_TARGET_TIMEOUT_S = float(os.environ.get("NO_TARGET_TIMEOUT_S", "1.5"))

# Distance→effect ONSET (cm) — the single install-day knob shared by every
# consumer of the distance feed. At/within this distance there is no
# distance-induced distortion (visual twist + bitmap) and the tape sits at its
# subtle default; the effect grows from here out to the far reach. Published as
# `distance_near_cm` so the browser and the Daisy tape bridge read one value
# (no JS edits, no Rust rebuild). Env-overridable for tuning on the wall, e.g.
#   DISTANCE_NEAR_CM=80 ./run_kiosk.sh
# NOTE: distinct from VL53_NEAR_CM above (a legacy 25 cm "present" reference).
DISTANCE_NEAR_CM = float(os.environ.get("DISTANCE_NEAR_CM", "75"))

# --- Empty-room distance learning -------------------------------------------
# The far end of every distance→effect mapping (the "full destruction" point,
# published as distance_far_cm) should be the ACTUAL empty-room reading, not a
# fixed guess. The VL53L1X reflects off whatever surface it faces, so even a
# clear line of sight may return a finite distance (e.g. ~350 cm off a far
# wall) rather than no-target. We learn that background live from the feed:
# when the smoothed distance holds essentially still — velocity below the
# threshold — for longer than the stillness window, nobody is moving in the
# cone, so the current reading IS the room, and we adopt it as the far reach.
#
# Re-learned continuously so it tracks the room over time (furniture moved, the
# sensor nudged or repositioned post-bootup, a new backdrop). Adoption is
# deliberately asymmetric: a stable reading FARTHER than the current estimate
# is trusted immediately (a clearer line of sight can only mean the previous
# estimate was occluded), while a CLOSER one is adopted slowly — a visitor
# standing dead-still is closer than the wall and must NOT pull the baseline
# in, or the empty room would read as "someone's here" forever.
EMPTY_ROOM_LEARN = True
# Velocity (cm/s) at/below which the scene counts as "not moving." Sized for
# sensor vibration / thermal drift (sub-cm/s), well under human motion. Gauged
# as the peak-to-peak distance excursion over the stillness window ÷ window,
# which resolves sub-cm/s (a per-sample derivative is swamped by jitter at
# 50 Hz) and is robust to small zero-mean wobble.
EMPTY_ROOM_VELOCITY_CM_S = float(os.environ.get("EMPTY_ROOM_VELOCITY_CM_S", "0.8"))
# How long (s) velocity must stay below the threshold before the current
# reading is accepted as the empty-room background. The request's ">10 s".
EMPTY_ROOM_STILLNESS_WINDOW_S = float(os.environ.get("EMPTY_ROOM_STILLNESS_WINDOW_S", "10.0"))
# Once a still scene persists, don't re-adopt more often than this (s). Keeps
# the estimate updating "periodically" without thrashing every frame.
EMPTY_ROOM_RELEARN_S = 5.0
# Smallest distance (cm) that can plausibly be the empty room — the minimum
# below which a still reading is NEVER assigned as the max/far ("empty-room")
# distance. A stable reading nearer than this is treated as a present,
# motionless subject — not the room — so someone standing still and watching
# the exhibit can't become the max distance, and it never lowers the baseline.
# Lower it for a tight install (sensor close to a wall); raise it if there is
# no near clutter to ever read legitimately.
EMPTY_ROOM_MIN_CM = float(os.environ.get("EMPTY_ROOM_MIN_CM", "200.0"))
# EMA weight (0..1, applied per re-learn) used when a confirmed empty-room
# reading is CLOSER than the current estimate (a genuine layout change). Small
# = slow: a single still visitor barely moves the baseline and it recovers once
# a farther stable reading reappears after they leave.
EMPTY_ROOM_DOWN_ALPHA = 0.25

# --- VL53L5CX (multizone) ---------------------------------------------------
# Used when VL53_SENSOR selects it. No short/long mode like the L1X — a single
# ~4 m range. The zone grid is reduced to one distance by taking the closest
# valid zone in the cone, so it publishes the same `distance_cm` topic.
VL53L5CX_RESOLUTION = 16    # 16 = 4x4 (up to 60 Hz), 64 = 8x8 (up to 15 Hz)
VL53L5CX_RANGING_HZ = 15
VL53L5CX_FAR_CM = 400.0     # far reach + no-target snap (published as distance_far_cm)
# Which zones form the "cone." None = every zone (closest target anywhere in
# the FoV). To ignore edge zones grazing the wall/floor, set a tuple of indices
# (row-major, 0..15 for 4x4 / 0..63 for 8x8) — e.g. the central 2x2 of a 4x4
# is (5, 6, 9, 10).
VL53L5CX_CONE_ZONES = None

# HR202 / TLC555 breath detection
BREATH_WINDOW_S = 0.2       # measurement window
BREATH_WARMUP_S = 10.0      # collect baseline, no detection
BREATH_BASELINE_ALPHA = 0.02
BREATH_TRIGGER_RATIO = 1.3  # fast_freq > baseline * this -> event
BREATH_DEBOUNCE_S = 3.0

# Ingest
INGEST_URL = "http://127.0.0.1:8080/ingest"
INGEST_BATCH_MS = 50
INGEST_QUEUE_MAX = 1024
INGEST_TIMEOUT_S = 1.0
