# Plan: Tape failure knob — single-param "destroy" control

One scalar in `[0, 1]` that drives 9 tape-stage parameters in concert,
producing a smooth lerp from **pristine TC-250** (current default) to
**eaten/failing tape**. Routable to any MIDI CC via the existing
`Param` enum, so the kiosk's distance sensor can ultimately control it
via the kiosk → MIDI bridge.

## What gets lerped

Pristine = current TC-250 preset (so `set_failure(0.0)` exactly restores
the present sound). Destroyed = empirically chosen extremes that hold
together as a cohesive "this tape is dying" gestalt rather than
sounding like 9 unrelated effects bolted together.

| Stage | Pristine | Destroyed | Curve |
|---|---|---|---|
| `wow_flutter.wow_rate_hz` | 0.5 | 3.0 | linear |
| `wow_flutter.wow_depth_ms` | 0.3 | 15.0 | quadratic (`a²`) — slow, then runs away |
| `wow_flutter.flutter_depth_ms` | 0.05 | 1.5 | quadratic |
| `chew.depth` | 0.1 | 0.9 | linear |
| `chew.freq` | 0.1 | 0.8 | linear |
| `loss.speed_ips` | 7.5 | 1.5 | linear (inverse on perception, so feels accelerating) |
| `loss.spacing_um` | 3.0 | 18.0 | linear |
| `hysteresis.drive` (cooked param) | 0.4 | 0.95 | linear |
| `hiss_amount` (linear gain) | 0.0032 | 0.025 | quadratic |

The curves matter — linear interpolation between subtle and extreme
ranges gives a knob that "does nothing for the first 30 %, then
suddenly everything." Quadratic on the depth/level params spreads the
useful range across the full sweep.

## Current state

- `TapeProcessor` has individual setters for every param above.
- `preset_sony_tc_250()` applies on `new()`, locking the pristine state.
- `MidiMap` + `Param` enum already in place. 8 variants today.
- `Engine::apply_param` dispatches CCs to drum and tape params already.

## Implementation order

1. **`TapeFailureSnapshot` struct** in `tape/mod.rs` holding the 9 numeric
   fields. Two `const`s: `FAILURE_PRISTINE` and `FAILURE_DESTROYED`.
   `FAILURE_PRISTINE` must mirror `preset_sony_tc_250()` exactly so
   `set_failure(0.0)` is the no-op recreation of the preset.
2. **`TapeProcessor::set_failure(amount: f32)`** — clamps to `[0,1]`,
   stores in `target_failure` field. Does *not* immediately push to
   the sub-stages — smoothing happens per-block (see below).
3. **Internal smoothing**: add `current_failure: f32` and `failure_smooth: f32`
   (the per-block coefficient) to `TapeProcessor`. At the top of
   `process()`, advance `current_failure` toward `target_failure`,
   then write the lerped params into each sub-stage. Smooth time
   ~50 ms.
4. **`Param::TapeFailure`** in `dsp/src/midi_map.rs`. Add the arm in
   `Engine::apply_param`:
   ```rust
   Param::TapeFailure => self.tape.set_failure(value),
   ```
5. **Host binding for dev/testing**: in `crates/host/src/main.rs`,
   add `m.bind_cc(?, Param::TapeFailure, 0.0, 1.0)` on a free CC
   number so a knob can drive it during testing.
6. **Sensor binding (future)**: the kiosk's distance sensor publishes
   over SSE today. The existing Pi → MIDI bridge (or a new one if not
   present) translates that to a CC on the same channel the Daisy is
   already listening to. No firmware change required — distance just
   appears as a CC and routes through `MidiMap`.

## Complications

1. **Pristine must equal TC-250 preset value-for-value.** Easiest way:
   in `preset_sony_tc_250()`, after configuring everything, take the
   resulting state as the source of truth and inline-paste those numbers
   as the `FAILURE_PRISTINE` const. Or compute pristine programmatically
   by snapshotting after preset apply. Either way, write a unit test:
   `set_failure(0.0)` after a fresh `new()` produces a no-op (current
   == pristine).
2. **Smoothing creates a perceptible knob lag.** 50 ms time constant is
   imperceptible on slow gestures but feels "stuck" on fast sweeps.
   Tradeoff against click-free hysteresis-drive and loss-filter
   transitions. If lag is annoying, **smooth only the click-prone params**
   (hysteresis drive, loss filter FIR) and let wow/chew/hiss snap.
   More code, more responsive.
3. **Linear MIDI CC granularity (128 steps).** Spreads across 0.0..1.0
   = 0.0078 per step. Audible per-step jumps are likely only on
   `chew_depth` near the low end (0.1 → 0.108 might cross a state-
   machine threshold). Test by ear; if jumpy, add a secondary smoothing
   stage on `chew_depth` specifically.
4. **Param interactions are nonlinear and intentional.** Crank
   hysteresis drive + loss filter together and the distortion becomes
   muffled rather than crispy. That's the "tape eaten" feel. Test the
   lerp at amount = 0.25, 0.5, 0.75, 1.0 by ear and confirm each step
   sounds progressively worse without losing musicality entirely.
5. **`set_failure` overwrites individual setters.** If the user calls
   `set_failure(0.5)`, then manually `set_hysteresis(...)`, then
   `set_failure(0.5)` again, the manual hysteresis tweak is lost.
   This is expected — `set_failure` is the master control. Document it
   so future-Chris doesn't get surprised.
6. **Hot-reload risk**: if `current_failure` is in an extreme state
   when the user reloads the .pat file or restarts MIDI, the next
   block could push huge param swings through the smoother. Initialise
   `current_failure` to 0.0 in `new()`; rely on smoothing to settle
   on first start.
7. **CC polarity / curve for the distance sensor.** "Closer = more
   broken" vs "Farther = more broken" is an artistic choice. Use
   `MidiMap::bind_cc(cc, Param::TapeFailure, min, max)` with
   `min > max` to invert if needed. No firmware change required.
8. **MIDI-rate updates inside the audio loop.** CC messages arrive at
   ~50–200 Hz from a controller; we already handle them via the
   `handle_midi` path (mutex-locked, brief). `set_failure` itself is
   cheap (a few float multiplies); smoothing absorbs jitter.
9. **CPU cost.** Each block: 9 float lerps + 9 setter calls (some of
   which trigger biquad recompute). The loss filter FIR rebuild is the
   only expensive setter — it's currently O(order²) = ~20 K ops. At
   ~20 Hz CC update rate that's 400 K ops/sec = trivial on STM32H7.
   Worth flagging only because it's the largest single cost.
10. **Smoothing depends on `process()` being called.** If audio stalls,
    smoothing pauses. That's fine — no sound, no need to update params.
    The next `process()` resumes from where it left off.

## Out of scope

- Per-axis failure (e.g. "lots of wow, no chew"). Use individual setters
  for that.
- Inverse "tape repair" continuous effect. `set_failure(0.0)` is the
  binary version.
- Per-genre / per-machine failure profiles ("cassette dying" vs
  "reel-to-reel dying"). One curve for now; add presets later.
- Velocity-sensitive failure (e.g. brief failure burst on a MIDI note
  hit). Same `Param` infrastructure can do this later via a separate
  trigger.

## Files touched

```
crates/dsp/src/
├── tape/
│   └── mod.rs              # +TapeFailureSnapshot, +constants,
│                           #  +set_failure(), +internal smoothing,
│                           #  +process() smoothing apply
├── midi_map.rs             # +Param::TapeFailure variant
└── lib.rs                  # +Param::TapeFailure arm in apply_param

crates/host/src/main.rs     # +bind_cc(?, Param::TapeFailure, 0.0, 1.0)
                            #  for dev testing
```

## Verification checklist

- [ ] `set_failure(0.0)` after fresh `new()` produces a no-op
      (compare param values to TC-250 preset)
- [ ] Sweep `set_failure(0.0 → 1.0)` over 5 s by hand-tweaking a CC;
      no clicks audible
- [ ] At `amount = 0.5`, the result sounds *more broken* than
      `amount = 0.25`, not just *louder* or *darker*
- [ ] At `amount = 1.0`, the music is recognisably broken but still
      playable as music (not pure noise)
- [ ] CC bound to a MIDI knob changes the value smoothly when twisted
- [ ] Calling `set_hysteresis(0.9, 0.5, 0.5)` between two
      `set_failure(0.5)` calls leaves the second call's value
      authoritative
