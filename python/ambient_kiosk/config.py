"""Pin assignments + tuning constants — authoritative copy of hardware-handoff.md."""

# BCM pin numbers
PIR_PIN = 4         # AM312 OUT
BREATH_PIN = 17     # TLC555 pin 3 (OUTPUT) — frequency counter input
TOUCH_IRQ_PIN = 27  # MPR121 IRQ

# I2C addresses (shared bus on GPIO2/GPIO3)
VL53L1X_ADDR = 0x29
ADS1115_ADDR = 0x48  # unused, reserved
MPR121_ADDR = 0x5A

# AM312
PIR_BOOT_SUPPRESS_S = 60.0  # ignore output for the first 60s post-process-start

# VL53L1X
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
# tuned on-site: run test_vl53l1x.py with the projector ON, on the real wall,
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
