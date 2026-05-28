# Tape Simulation — implementation status

Port of [CHOWTape](https://github.com/jatinchowdhury18/AnalogTapeModel)
(Chowdhury's analog tape model) into `dsp/`, leaning on
[`infinitedsp`](https://github.com/Na1w/infinitedsp/) primitives where the
algorithm is generic and writing custom Rust for the physics-driven parts.

Sources: CHOWTape [wiki](https://github.com/jatinchowdhury18/AnalogTapeModel/wiki/User-Manual),
the [DAFx 2019 paper](http://dafx2019.bcu.ac.uk/papers/DAFx2019_paper_3.pdf),
and the [VST C++ source](https://github.com/jatinchowdhury18/AnalogTapeModel/tree/master/Plugin/Source).

## Status

| Phase | Component | State |
|---|---|---|
| 1 | Head bump (peaking biquad) | ✅ implemented |
| 1 | Hiss (noise + LPF) | ✅ implemented |
| 2 | Wow + flutter | ✅ implemented |
| 3 | Loss filter (FIR) | ✅ implemented |
| 4 | Hysteresis (Jiles-Atherton + RK2) | ✅ implemented |
| 5 | Chew (dropouts) | ✅ implemented |
| 5 | Bus compressor | ✅ implemented (`infinitedsp::Compressor`) |
| – | Sony TC-250 preset | ✅ default in `TapeProcessor::new()` |

## Current signal chain (master bus)

```
sampler + kick → reverb → hysteresis → wow/flutter → loss filter
              → head bump → chew → +hiss → bus compressor → output
```

Order matches CHOWTape's record-then-playback convention: hysteresis
(record head saturation) before linear filters; chew/hiss/compressor as
post-playback artifacts.

## Component summary

| Component | Source | What we did |
|---|---|---|
| Hysteresis | `tape/hysteresis.rs`, ~160 LOC | JA equations + RK2 solver, scalar f32, no SIMD, no oversampling. CHOWTape's `cook(drive, width, sat)` math is direct port. |
| Loss filter | `tape/loss_filter.rs`, ~145 LOC | 3-term frequency-domain magnitude → IDFT-by-summation → symmetric FIR. ~140 taps at 48 kHz (scales with SR). Doubled-length ring buffer for modulo-free convolution. |
| Head bump | `tape/mod.rs` (inline) | `infinitedsp::Biquad::Peaking` per channel. Fixed 80 Hz / Q=2 / +3 dB. |
| Wow + flutter | `tape/wow_flutter.rs`, ~170 LOC | Shared modulation signal (1 cos + drift for wow, 3 summed cos for flutter), independent fractional delay lines per channel. 12 ms base delay headroom. |
| Hiss | `tape/mod.rs` (inline) | `Oscillator(WhiteNoise)` → `Biquad::LowPass` at 7 kHz → mixed into both channels. |
| Chew | `tape/chew.rs`, ~150 LOC | State machine with random dry/wet durations + `sign(x)·\|x\|^p` shaper + one-pole LPF whose cutoff drops to 5 kHz during crinkle. |
| Compressor | `tape/mod.rs` (inline) | Two `infinitedsp::Compressor` (one per channel). −6 dB threshold, 1.8:1, 10/100 ms attack/release, soft knee, +1.5 dB makeup. |

## TC-250 preset

`TapeProcessor::new()` applies `preset_sony_tc_250()` automatically.
Settings derived from the published Sony datasheet:

| Param | Value | Datasheet |
|---|---|---|
| `set_speed_ips` | 7.5 | High-quality mode |
| `set_spacing_um` | 3.0 | Assumed mild wear (not in datasheet) |
| `set_thickness_um` | 30.0 | Assumed 1.5 mil consumer tape (not in datasheet) |
| `set_gap_um` | 10.0 | Assumed permalloy-era typical (not in datasheet) |
| Hysteresis `drive` / `width` / `sat` | 0.40 / 0.50 / 0.30 | Targets <1% THD spec at 0 dB |
| Wow rate / depth | 0.5 Hz / 0.3 ms | ≈0.094% peak deviation, contributing to 0.19% W&F spec |
| Flutter depth | 0.05 ms | ≈0.22% peak deviation at 7 Hz, combined ≈0.19% spec |
| Hiss amount | 0.0032 linear | 50 dB S/N spec |
| Chew `depth` / `freq` | 0.1 / 0.1 | Subtle dropouts every ~3-4 s, ~75 ms long |

## Known limitations

### Audio quality

1. **No oversampling on hysteresis.** JA runs at audio rate; the
   nonlinearity aliases on bright transients (cymbal hits, the kick's
   click attack). CHOWTape oversamples 4-8× for this reason. Fix:
   port a polyphase resampler, run hysteresis at 4× SR.
2. **No oversampling on chew either.** The `|x|^p` shaper also
   aliases, less audibly because it only activates during dropouts.
3. **No crossfade on loss-filter param changes.** Twisting `speed_ips`
   / `spacing_um` / `thickness_um` / `gap_um` rebuilds the FIR
   coefficients and substitutes them immediately — you'll hear a
   click. CHOWTape uses a 1024-sample crossfade between two FIR
   instances. Fix: keep two FIR instances, fade between them on
   param change.
4. **No DC blocker.** Hysteresis can introduce DC offset on
   asymmetric input. Fix: append a high-pass at ~20 Hz post-chain.
5. **No bypass smoothing.** `TapeProcessor::set_enabled(false)` does
   a hard cut — click on toggle. Fix: smooth a wet/dry coefficient.

### Modeling fidelity

6. **Head bump not coupled to speed/gap.** CHOWTape's head bump
   centre frequency is computed as
   `speedIps · 0.0254 / (gapMeters · 500)`. Ours is fixed at 80 Hz.
   Side effect: changing `set_speed_ips()` doesn't move the head
   bump's resonance. Fix: recompute bump biquad when speed/gap change.
6. **No input filters.** CHOWTape has a pre-tape EQ stage that
   shapes the signal hitting the record head. We feed raw audio in.
   Audible only on very bright sources.
7. **No mid/side mode.** CHOWTape can process M/S separately.
   Ours is L/R only. Affects stereo width in extreme settings.
8. **No bias adjustment.** Real tape has a bias parameter (high-
   frequency excitation that linearises the record curve). CHOWTape
   models it; we don't. The hysteresis behaves as if bias is fixed.
9. **Hiss is correlated stereo.** Both channels get identical noise.
   Real tape tracks have independent magnetic noise; CHOWTape uses
   decorrelated noise. Cosmetic for mono-derived sources.
10. **f32 throughout JA model** vs CHOWTape's f64. The `near_zero`
    branch protects against the most precision-sensitive case (`coth -
    1/Q` at small Q) but other places may quietly lose accuracy.
    Not yet observed misbehaving but flagged for future audit.

### TC-250 preset accuracy

11. **Head gap, tape thickness, and head-to-tape spacing are
    guesses,** not from the Sony datasheet (which doesn't specify
    them). They're educated picks based on era and machine class.
    A definitive answer would require either a service manual that
    lists head dimensions or measured frequency response of a real
    unit.
12. **Hysteresis settings (`drive=0.40, width=0.50, sat=0.30`) target
    the <1% THD spec at line level by ear**, not by fitting to a
    measured curve. Closer match would need a recorded reference.

### Architecture / performance

13. **Loss FIR allocates ~1 KB per param change** for the intermediate
    magnitude buffer. Rare enough not to matter, but on Daisy with
    the 64 KB heap it's a momentary spike.
14. **Compressor params are global** — single threshold/ratio/etc.
    for the whole stereo bus. No sidechain, no input-level-following.
    Fine for "tape glue", insufficient for "musical mastering bus".
15. **DSP latency accumulates** through the chain. Wow/flutter adds
    ~12 ms (base delay headroom); loss FIR adds another ~1.5 ms
    (order/2 at 48 kHz). Total ~13.5 ms added to master bus.
    Imperceptible for non-realtime-monitoring playback; would be
    problematic for live monitoring.
16. **Performance not measured on Daisy hardware.** All CPU estimates
    are paper-budget. Firmware target builds clean but hasn't been
    flashed and profiled yet.
17. **No unit tests.** Every module lacks tests. Future regressions
    in coefficient generation or solver stability won't be caught
    until the user listens.

## What's intentionally not ported

- **CHOWTape's STN neural-net solver** for hysteresis. It needs a
  pre-trained model file we'd have to convert + reimplement;
  RK2 is fast enough.
- **CHOWTape's V1 mode** (vintage parameter set with M_s scaled by
  50000×). Tape model variant we don't need for TC-250.
- **CHOWTape's RK4 / Newton-Raphson solvers.** RK2 is sufficient
  and 2-4× cheaper.

## Files

```
crates/dsp/src/
├── tape/
│   ├── mod.rs              # TapeProcessor + chain orchestration + TC-250 preset
│   ├── hysteresis.rs       # JA model + RK2 (Phase 4)
│   ├── loss_filter.rs      # FIR builder + convolver (Phase 3)
│   ├── wow_flutter.rs      # LFOs + delay-line modulation (Phase 2)
│   └── chew.rs             # State machine + power-law shaper (Phase 5)
└── lib.rs                  # `pub mod tape;` + Engine integration
```

Engine integration: `Engine::process` calls
`self.tape.process(output, sample_index)` after the reverb wet/dry
blend, as the last stage before the output buffer is written.

## Public API surface

User-facing setters on `dsp::TapeProcessor`:

```rust
// Master switch
set_enabled(bool)

// Hysteresis (saturation character)
set_hysteresis(drive: f32, width: f32, sat: f32)  // each 0..1

// Loss filter (physical playback losses)
set_speed_ips(f32)         // 1-50
set_spacing_um(f32)        // 0.1-20
set_thickness_um(f32)      // 0.1-50
set_gap_um(f32)            // 1-50

// Hiss
set_hiss_amount(f32)       // 0..1, linear

// Chew (dropouts) — access via chew_mut()
chew_mut().set_depth(f32)
chew_mut().set_freq(f32)
chew_mut().set_variance(f32)
chew_mut().set_enabled(bool)

// Wow + flutter — access via wow_flutter_mut()
wow_flutter_mut().set_wow_rate_hz(f32)
wow_flutter_mut().set_wow_depth_ms(f32)
wow_flutter_mut().set_flutter_depth_ms(f32)
wow_flutter_mut().set_enabled(bool)

// Bus compressor
set_compressor_enabled(bool)
set_compressor(thr_db, ratio, atk_ms, rel_ms, makeup_db)

// Presets
preset_sony_tc_250()       // applied automatically in `new()`
```
