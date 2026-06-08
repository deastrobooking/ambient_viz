# Backlog — future improvements

Canonical, version-controlled backlog for the separate audio-instrument rebuild
fork. Captured from design sessions plus sweeps of repo docs and notes. The live
agent task list is session-scoped; **this file is the permanent home** — update
it here.

Each item links its source doc/memory. Dependencies noted inline. Completed
verification checklists from already-shipped work (USB composite Phases A–D, tape
failure live control, SAI audio path) are intentionally excluded.

## Audio-first fork priorities

- [x] **Condensed audio-fork docs and agent memory** — `AGENT_MEMORY.md` now
  owns current state, donor research, and the milestone plan; architecture and
  donor docs are shortened to focused references. — `AGENT_MEMORY.md`,
  `AUDIO_ENGINE_FORK.md`, `SYNTH_SUITE_IMPORT_PLAN.md`, `GROOVEBOX_REPURPOSE.md`
- [ ] **Host groovebox harness** — first stdin-command pass exists: host reads
  the shared text protocol, applies `GrooveEvent`, starts a fixed 120 BPM loop
  without a timeline, and makes old bloom/freeze audition modulation opt-in via
  `--test-mod`; `HELP` and `STATE` now print host-side command help and engine
  snapshots including macro/filter state. Next: keyboard/serial/MIDI ergonomics
  and a more playable TUI/shortcut layer. —
  `AUDIO_ENGINE_FORK.md`, `GROOVEBOX_REPURPOSE.md`
- [ ] **Groovebox control protocol** — `groove::parse_line` now defines the first
  shared text command set for `PLAY`, `STOP`, `RESET`, `PAD`, `TOGGLE`, `STEP`,
  `MACRO`, `TRACK`, `BAND`, and `FILTER`; selected-band filter controls are now
  routed through `Engine::handle_groove_event`. Next write a compact
  CDC/MIDI/UART event encoder only after the text protocol feels good. —
  `AUDIO_ENGINE_FORK.md`, `SYNTH_SUITE_IMPORT_PLAN.md`
- [ ] **Pattern bank + editing model** — fixed 8-slot `PatternBank` now captures,
  loads, copies, clears, fills, and deterministically randomizes drum/stab lanes
  through `GrooveEvent` commands (`PATTERN`, `CAPTURE`, `PCOPY`, `PCLEAR`,
  `PFILL`, `PRAND`); `BASS`/`PBASS` edit live or slotted bass rest/hold/strike
  cells; `PATTERN` queues mid-loop loads to step 0. Next: minimal project
  snapshot format. — `AUDIO_ENGINE_FORK.md`
- [ ] **Synth/filter/modulation expansion** — selectively port small
  Nexus12/WolfGang ideas: one oscillator/filter at a time, fixed modulation
  matrix, macro scenes, and tests. Avoid plugin/desktop runtime assumptions. —
  `AUDIO_ENGINE_FORK.md`, `SYNTH_SUITE_IMPORT_PLAN.md`
- [ ] **Standalone Spectre filter core** — master-filter insert and standalone
  dynamic band rack have landed (`Off`, `Clean LP`, `Ladder12`, `Ladder24`,
  `Diode`, `SEM Morph`; 8 biquad bands; Stereo/Mid/Side/Left/Right modes;
  envelope-followed gain/cutoff). `MACRO filter_cutoff`, `filter_resonance`,
  and `filter_motion` steer the selected band through the shared protocol;
  `BAND` and `FILTER` commands provide selected/explicit band editing, and host
  `STATE` displays selected-band envelope activity. Next: transient/color
  models and host-side analyzer snapshots. —
  `SYNTH_SUITE_IMPORT_PLAN.md`
- [ ] **Standalone Daisy groovebox build** — codec line out first, hardware
  controls into `Engine::handle_groove_event`, project/pattern/sample storage,
  visual/Pi sync optional. — `AUDIO_ENGINE_FORK.md`, `daisy/README.md`
- [x] **Pi 4 audio-fork deployment guide** — `PI4_AUDIO_TEST_DEPLOYMENT.md`
  defines the Pi as a companion for mock SSE, sensors, Daisy CDC
  song-position/control, and visual sync; analog Daisy line out remains the
  audio acceptance path. — `PI4_AUDIO_TEST_DEPLOYMENT.md`

## Audio capture / transport

- [ ] **Run the USB-capture diagnostic** — flash `debug-uart`, briefly revive
  `getUserMedia` capture, read `usb_drop`/`usb_pktmax` on the `diag:` heartbeat to
  decide Daisy-side (missed polls) vs Pi-side (PipeWire clocking) failure. *Gates
  the two below.* — `daisy/PLAN_USB_CAPTURE.md`
- [ ] **Pi-side capture quick wins** *(only if failure is Pi-side; needs diagnostic
  first)* — USB autosuspend off → PipeWire quantum/clock config → RT priority/affinity
  on PipeWire+Chromium. Skip RT kernel + static IP. — `daisy/PLAN_USB_CAPTURE.md`
- [ ] **WebUSB vendor-BULK capture spike** *(needs diagnostic first)* — expose audio
  over a class-0xFF bulk IN endpoint, read via `navigator.usb`→`transferIn`→AudioWorklet;
  bypasses the PipeWire/Chromium capture graph and makes SD stalls benign. Measure
  flash delta vs the UAC code. — `daisy/PLAN_USB_CAPTURE.md`, mem `daisy-usb-capture-revival`
- [ ] **Rust `dasp` DSP/analysis sidecar** — move FFT/envelope/transient analysis out
  of the browser into a native Rust process feeding the visualizer over the SSE/WebSocket
  bridge. Decouples from Chromium's audio stack; pairs with the WebUSB path. Start from
  the daisy `host` crate. — conversation 2026-06-06 (new)

## Firmware / DSP (Daisy)

- [ ] **async/DMA SD reads** — non-blocking SDMMC the audio task can `await` +
  double-buffering, so SD reads stop freezing the embassy executor (root cause of the
  USB iso clicks). Interim: contiguous-sector reads to make each read uniform <1 ms. —
  mem `daisy-uac-async-sd-future`, `daisy-usb-capture-clicks`
- [ ] **Bootloader + QSPI XIP** — run firmware from external QSPI via the Daisy
  bootloader to lift the 128 KB internal-flash ceiling; then revert the `opt-level='z'`
  debug-alias workaround. Prereq for large additions (embassy-net, etc.). —
  `daisy/PLAN_QSPI_BOOTLOADER.md`, mem `daisy-qspi-flash-future`
- [ ] **Phase E: inbound sensor→MIDI over CDC** — host→device sensor data as MIDI CC so
  a sensor drives Daisy audio (TapeFailure) in lockstep with the visual. Deferred at
  `usb_cdc.rs:16`. — `daisy/PLAN_USB_COMPOSITE.md` Phase E
- [ ] **Tempo Pi→Daisy (or onboard `bpm_at`)** for the dsp Sequencer — parked until the
  sequencer is instantiated; prefer onboard `bpm_at(own POS)` over a tempo CC. —
  mem `daisy-tempo-sequencer-future`
- [ ] **Tape model quality** — oversampling (hysteresis, chew shaper), FIR crossfade on
  loss-filter changes, DC blocker, bypass smoothing, head-bump↔speed coupling, pre-tape
  EQ, mid/side, bias param, decorrelated stereo hiss, JA f32 audit. — `daisy/TAPE_SIMULATION.md`
- [ ] **Tape DSP unit tests** — regression tests on the Mac host (no-op `set_failure(0)`,
  monotonic brokenness, loss-FIR correctness, JA precision branch). — `daisy/TAPE_SIMULATION.md`
- [ ] **Multichannel I/O (4×stereo TDM)** *(speculative)* — AK5558/AK4458 availability,
  SAI pin routing, 8-slot TDM config, I²C init. — `daisy/MULTICHANNEL_IO.md`
- [ ] **Synth/sampler Engine path** *(optional, non-exhibit)* — `Engine::handle_midi`
  (currently a sine stub), dsp sampler, host MIDI input. Confirm it's wanted first. —
  `daisy/README.md` roadmap

## Sensors

- [ ] **ESP32 wireless sensor network** — ESP-NOW satellites (ESP32-C3) → ESP32-S3 host →
  Pi over USB CDC. Prototype one node→host→Pi; measure real in-enclosure ESP-NOW range;
  decide detection-logic split + battery vs wired; deterministic USB enumeration. —
  `ESP32_SENSOR_NETWORK.md`, `TOUCH_EXPANSION.md` Option B
- [ ] **Multi-MPR121 wired touch expansion** — extend `touch.py` to multiple boards over
  extended I²C; grow TOUCH_COLORS/TOUCH_ENV + worker mapping. Wired alternative to ESP32
  satellites. — `TOUCH_EXPANSION.md` Option A, mem `kiosk-mpr121-mapping`

## Rendering / performance

- [ ] **Measure the render bottleneck** — `?bitmap=N` FPS sweep (scaling ⇒ upload-bound)
  + direct-scanout check (`WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` A/B; `labwc -d | grep scan`;
  `sudo cat /sys/kernel/debug/dri/<vc4>/state`). *Gates native eval.* —
  `PI_PERFORMANCE.md`, conversation 2026-06-06
- [ ] **Eliminate per-frame `texImage2D(canvas)` upload** — migrate remaining Canvas2D
  compositing to FBO/WebGL-resident rendering (the dominant Pi-4 GPU-bandwidth cost). The
  higher-ROI alternative to a native rewrite. — `PI_PERFORMANCE.md`
- [ ] **Evaluate a native wgpu renderer** *(only if still GPU-bound after FBO work +
  measurement)* — gain is GPU-residency + dropping Chromium's command-buffer tax, NOT
  fewer compositor ops; it's a full renderer rewrite. — mem `viz-native-wgpu-tradeoff`
- [ ] **Remaining Canvas2D micro-optimizations** — the unchecked `[ ]` items in
  `OPTIMIZATIONS.md` (#3,4,6–14: lattice integer coords, grain pre-bake, gradient cache,
  Float32Array, globalAlpha, save/restore trim, etc.).
- [ ] **Runtime-tunable render knobs** — expose FLYOUT_COUNT, SCANLINE_PERIOD (const today),
  wire up ED_TOOLBAR_H. — `PI_PERFORMANCE.md`, `static/index.html`

## Kiosk hardware

- [ ] **Addressable LED strip/array output** — drive WS2812/SK6812 from audio/visual state
  (Pi SPI vs ESP32 node vs Daisy); define layout + data source (palette/levels via SSE or
  the dasp sidecar). — conversation 2026-06-06 (new)
- [ ] **Finalize cursor hiding on labwc** — transparent XCURSOR_THEME for the compositor
  default (mouseless case), plus the USB-mouse + page-cursor sources; verify on hardware. —
  `PI_KIOSK_BRINGUP.md`, mem `kiosk-hide-cursor-wayland`
- [ ] **Enclosure: measurements + print fixes** — board/jack/USB/cable/Dupont measurements;
  fix undersized holes, snap-fit, edge stringing. — `ENCLOSURE.md`, `MODEL_NOTES.md`

## Visualizer features / interaction

- [ ] **Proximity→effect direction config flag** — replace the hardcoded reversal (near =
  distorted, ce577ea, 3 ramps/2 files) with one flag; share with the Phase E audio leg. —
  mem `distance-reverse-flag-future`
- [ ] **Build out unbuilt EXHIBIT interactions** — B dwell-destabilizes, D buzzer/touch
  stabs, E humidity→reverb, F floor-pad beats, G spatial zones, H eavesdropping cone; plus
  catch-delay tap + SVF bloom bank. Suggested first build A+C+D. — `EXHIBIT.md`

## Infra

- [ ] **Multi-project support** — generalize from the single hardcoded arrangement to a
  project manifest (audio, timeline/lanes, sensor mappings, palettes, localaudio source) +
  a selector; make the bridge + Python sidecar project-aware. — conversation 2026-06-06 (new)
