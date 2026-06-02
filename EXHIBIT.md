# The exhibit — concept, components, primitives, interaction ideas

> Working notes for the installation. Captures *what the piece is*, *what
> hardware/software makes it up*, *what audio + sensing primitives exist to
> build on*, and an open brainstorm of ways attendees could introduce sound
> without a traditional MIDI controller.

## Concept

**Walking down the street at night, hearing noises leaking out of different
clubs.** The backing track is composed to elicit that feeling — fragments of
rooms you pass but never fully enter, each with its own pulse, bleeding
through walls and over the city's hum.

Thematically the work sits in *Pain Material* territory — paranoia,
surveillance, alienation, dissociation. That frame matters for interaction
design: the most fitting interactions are **environmental and deniable**. The
space *senses* the attendee — their presence, proximity, touch — and reshapes
the sound whether or not they meant to play it. The visitor is being
monitored, and their behaviour is composing without consent. That's the point,
not a side effect.

Design rule that falls out of this: **no instrument on a pedestal.** No
keyboard, no pad controller, no "play me" sign. The controller is dissolved
into the architecture — the body and its movement *are* the interface.

### Composition structure (important)

The backing track is **not a song** — it's an **18-minute ambient
composition**. The metaphor is literally baked into it:

- **Ambiance** — lengthy stretches of subtle pads + rain / street sounds. This
  is the *street* between the clubs, and most of the runtime.
- **Four "songs"** — heard **from afar**, *within* the ambiance. They
  **fade / filter in and out** under their own composed envelopes. These are
  the four *clubs* you pass.

Consequences for interaction design:

- **The piece already authors its own "distance."** The composed fades/filters
  are the artist's sense of near/far. A global distance→`TapeFailure` would
  stack a *second* distance on top — redundant unless reframed (see "Resolving
  vs. re-fading" below).
- **There is no single key or tempo over 18 minutes.** Each of the four songs
  has its own; the ambiance is largely atonal (pads + noise). Anything that
  must be *consonant* (SVF tuning, tier-3 added voices) has to follow the
  **current section**, not one global value. Tempo already tracks the timeline
  BPM curve; a per-section **key lane** in the timeline would be needed for
  pitch consonance.
- **Interaction should be position-aware.** The same gesture should mean
  different things by location in the piece: during ambiance it *summons tone
  from the rain/street*; during a distant song it *pulls that song closer*. The
  engine knows position via `sequencer().time_seconds()` (and the Pi via the
  CDC `POS` channel), so behavior can be gated by composition time.

## Components

```
                club fragments (the "street")
   composed   ┌──────────────────────────────┐
   backing  ──┤  audio bed (MP3 + timeline)   │
   track      └──────────────────────────────┘
                           │
   attendee presence       │            ┌─────────────────────────┐
   ┌──────────────┐  MIDI  │            │  Daisy coprocessor       │
   │ kiosk sensors│──CC───────────────► │  (dsp::Engine)           │
   │ PIR / ToF /  │  (sensor→MIDI       │  kick · hats · FM stabs  │
   │ humidity /   │   bridge)           │  · tape failure · delay  │
   │ MPR121 touch │                     │  · step sequencer        │
   └──────┬───────┘                     └────────────┬────────────┘
          │ SSE events                       codec out → PA (room)
          ▼                                  USB UAC  → Pi
   ┌──────────────┐                                   │
   │ visualizer   │ ◄──── audio (getUserMedia) ───────┘
   │ (B&W CRT/    │ ◄──── song position (CDC serial) ─┘
   │  glitch)     │
   └──────────────┘
```

- **Backing track** — `static/20251006_arrangement_1.mp3` + sidecar
  `.timeline.json` (BPM curve + visual-event lanes). An **18-min ambient
  composition**: ambiance (pads / rain / street) with **four songs heard from
  afar** fading/filtering in and out. See "Composition structure" above.
- **Visualizer** — `static/index.html`. Single-canvas B&W CRT/glitch render
  driven by FFT band analysis (bass/mid/treble/level → envelopes → slice
  tears, freeze, dither, flyout silhouettes). Reference points: NIN, Aphex
  Twin, Venetian Snares. Runs standalone or in kiosk mode on a Pi 4.
- **Kiosk sensors** (`hardware-handoff.md`, `SENSOR_MAPPING.md`) — Pi-based:
  - **AM312 PIR** — binary presence / motion.
  - **VL53L1X ToF** — distance (mm), auto-ranged from ambient IR at boot.
  - **HR202 + TLC555** — humidity, very slow.
  - **MPR121** — 12-channel capacitive touch / near-touch.
  Python sidecar reads them, POSTs JSON events to a Node SSE bridge, which
  relays to the browser (`window.AMBIENT_INPUTS`). Today they drive *visual*
  params (distance → twist amplitude, distance → bitmap resolution, etc.).
- **Daisy coprocessor** (`daisy/`) — Rust workspace. `no_std` `dsp` core shared
  by a macOS dev `host` (cpal/midir) and Daisy Seed `firmware` (embassy + USB
  UAC + UART-MIDI). Audio out goes to the room PA *and* over USB to the Pi
  visualizer. Sensor→audio path is **MIDI CC → `Param`** via the engine's
  `MidiMap` — so any sensor can already drive any audio knob through the
  planned kiosk→MIDI bridge.

## Audio + sequencing primitives (what's available to build on)

From `daisy/crates/dsp`. Everything below is buffer-size / sample-rate
agnostic and runs identically on host and firmware.

**Voices**
- **Analog bass drum** (`analog_bass_drum.rs`) → into a `Distortion`.
  Knobs: freq, accent, decay, tone, self-FM ("vrrm" pitch dive), attack-FM,
  distortion drive.
- **Hi-hats** (`hihat.rs`) — closed + open, shared gain.
- **FM stab bank** (`fm_stab.rs`) — 8-voice poly 2-operator FM, built for
  techno/industrial chord hits. Knobs: mod:carrier ratio (integer =
  harmonic/brassy, non-integer = metallic/clangorous), FM index (brightness),
  operator self-feedback (**the main grit lever** — sine → buzzing saw),
  pre-shaper drive, output shaper (`None`/`Tanh`/`HardClip`/`Foldback`), amp
  attack/decay, mod-env decay. Ships with a clean DX-ish default and an
  `industrial()` preset.
- **Sampler** — plays the backing buffer with linear-interp SRC.

**Effects**
- **Tape processor** (`tape/`) — Sony TC-250 emulation: wow/flutter, chew,
  loss filter, hysteresis (hiss disabled — too noisy by ear). Collapsed to a
  single **`set_failure(0..1)`** knob: **0 = the light-tape baseline**
  (`preset_light_tape` — "a touch of tape", halfway between clean and the full
  TC-250; the default), **1 = eaten / dying TC-250** (drives 9 sub-stage params
  on tuned curves — see `daisy/PLAN_TAPE_FAILURE.md`). This is the strongest
  single gesture for "distance / decay / dissolution."
- **Stab ping-pong delay** — wet, feedback (repeats), time.
- **Reverb** — wet/dry mix on the master.
- **Master freeze** (`freeze.rs`) — **parallel-send** grain hold, the audio
  mirror of the visualizer's frame-freeze. The live master keeps playing
  untouched; a looped ~0.3 s grain of it (two-head overlap-add, seam-aligned →
  click-free) is run through a stripped failure-tape (`GlitchTape`: wow/flutter
  + chew, gated to run only while frozen) and summed *under* the master at a
  fixed return trim, so a degraded wobbling ghost of a caught moment hovers over
  the continuing composition. A master peak limiter (`limiter.rs`) on the sum
  keeps the level ~unchanged from the dry master. Driven by `set_freeze(0..1)` /
  `Param::Freeze`. Audio engine built + tested; the driving transport (browser
  JS freeze → CDC, per `PLAN_USB_COMPOSITE.md` Phase E) is not yet connected —
  on the host a test thread holds ~0.5 s every ~10 s.

**Sequencing** (`sequencer.rs`, `chord.rs`)
- Step sequencer from `.pat` grid files. Per-step velocity lanes for **kick**,
  **chat** (closed hat), **ohat** (open hat), and a **stab** trigger grid.
- Separate **`prog`** chord lane: roman numerals diatonic to `key:`, absolute
  chord names (`Ebmaj7`, `F#m7`), or explicit `[C3 Eb3 G3]` voicings. Each
  stab trigger pops the next chord; restarts every loop (deterministic).
- Per-pattern resolution (`res:` — 8ths / 16ths / triplets), up to 128 steps.
- Tempo locked to the timeline BPM curve; mid-loop tempo changes are drift-free
  (per-sample `step_phase`).

**Routing**
- `MidiMap`: 128-entry CC → `Param` table, linear-mapped to `[min,max]`.
  Mappable `Param`s today: `KickFreq KickAccent KickDecay KickTone KickAttackFm
  KickSelfFm KickDistDrive ReverbWet StabGain StabIndex StabDecay StabModRatio
  StabFeedback StabDrive TapeFailure StabDelayWet StabDelayFeedback
  StabDelayTime`. Note-on (e.g. note 36) fires the kick.

**Sensor → audio mapping that's realizable with the *current* kiosk hardware:**

| Sensor | Signal | Natural audio target |
|---|---|---|
| VL53L1X ToF | distance (mm), continuous | `TapeFailure` / `ReverbWet` / `StabDelayWet` |
| AM312 PIR | presence (binary, edge) | beat gate, one-shot stab, "the room notices you" |
| MPR121 | 12 touch channels | 12 stab chords / `prog` advance / buzzer pads |
| HR202 | humidity, glacial | slow collective drift (`StabIndex`, reverb size) |

---

## Interaction brainstorm

Open ideas — none built yet. Tagged by which need only current hardware vs.
added hardware. Each ties a concrete sensor gesture to concrete primitives and
to the concept.

### A. Proximity resolves the club *(current hardware: ToF)*
Map **distance → `TapeFailure`** *inverted*: far away = high failure
(wow/flutter, hiss, muffled dropouts — a club heard through a wall and night
air); walk up close = pristine and present. Layer distance → `ReverbWet` and
`StabDelayWet` so approaching also pulls the sound *out* of a wash. The visitor
doesn't turn it *up*, they bring it *into focus* — which is more poetic and
exactly "hearing noises from a club as you near the door." This is the single
highest-leverage idea and works today.

**Resolving vs. re-fading — DECIDED: resolve.** The composition *already*
fades/filters its four songs by composed distance, so the visitor's approach
*pulls a window of clarity over the composed mix* — the resting state is the
piece as authored, and proximity sharpens whatever's currently up (withdrawal
returns it to the haze). This complements the composition instead of
duplicating its distance axis. The rejected alternative was *re-fade* (visitor
rides a song's existing fade up), which risked stepping on authored gestures.
Concretely: at rest, tape/filters sit at the composed value; approach drives a
*localized reduction* of `TapeFailure` (+ a touch of high-shelf / reverb-dry
bias) — clarity is subtractive over the existing mix, never additive.

### B. Dwell destabilizes *(current hardware: ToF)*
The longer someone lingers close, the further the `prog` chord lane drifts or
the key modulates — overstaying makes the harmony wander and curdle. Comfort
that sours if you don't move on. Alienation, on a timer.

### C. The room notices you *(current hardware: PIR)*
On first PIR detection after a quiet spell, fire a single loud `industrial()`
stab and snap tape to pristine for a beat — the club "turns to look." Then it
drifts back. Literal surveillance: presence is sensed and answered. Variant:
**gate the kick** to presence — the beat only runs while someone's there
(reads as "performs only when watched") or *stops* when watched (indifference /
paranoia). Pick the reading that unsettles more.

### D. Buzzer panel *(current hardware: MPR121)*
Hide the 12 capacitive electrodes under a surface that *looks* like something
you shouldn't touch — an intercom/door-buzzer panel, graffiti, a railing.
Each touch fires a stab chord (or advances `prog` one step). Map **hold
duration → `StabDecay`** so a long press leaves a chord ringing. Brief,
gestural, slightly transgressive — you pressed the buzzer; the building
answered.

### E. Collective heat *(current hardware: humidity)*
Map **humidity → `StabIndex`** (brightness) or reverb size, drifting over
*minutes*. As bodies fill the room and warm/humidify the air over the evening,
the timbre slowly opens up. Invisible, collective, unattributable — nobody
knows they're doing it, everyone is.

### F. Footsteps become the beat *(added hardware: floor pads)*
Floor proximity/pressure pads along a "street" walkway. A step in a zone writes
a transient velocity into the next kick/hat step in the sequencer grid — people
build the rhythm by *walking down the street*, the most on-concept gesture
available. Needs pads beyond the current sensor set (capacitive plates, FSRs,
or load cells).

### G. Passing the doorways — spatial club zones *(added hardware: multiple ToF/zones)*
Several proximity zones along a rail, each a *different club*: its own stab
patch, `prog`, and `.pat` pattern. Moving past crossfades club identities —
A's four-on-the-floor bleeds into B's inharmonic stab progression. The backing
track is the street; each zone is what leaks from one doorway. The fullest
realization of the core metaphor; needs more than one distance sensor (or a
multi-zone ToF mode / a row of MPR121 proximity pads).

### H. Eavesdropping cone *(added hardware: one tight beam / a listening tube)*
A horn or tube you put your head near, or a tight ToF beam at one spot. Getting
close *solos* a hidden stab layer or the chord progression — like leaning into
a cracked door. Public vs. private listening; who's leaning in, who's watching
them lean.

### I. The body is the controller *(framing, not a feature)*
State it as a principle: every `Param` is already sensor-drivable via CC, so
there is no controller — only architecture that senses. Worth foregrounding in
the wall text. The unease *is* the interaction.

### Suggested first build
**A + C + D** are fully realizable on the current kiosk hardware and together
cover the three core gestures — *approach* (ToF→tape failure), *presence*
(PIR→the room reacts), *touch* (MPR121→stabs). That's a complete, on-concept
interactive layer with zero new hardware. F/G/H are the expansion path if the
physical build grows.

---

## Constraint: the backing track is already a complete composition

The bed is finished and self-sufficient. Any *additive* layer (a running
synth pattern, a competing kick pulse) fights the mix. So the design goal is
not "add an instrument" — it's to make interaction **process and excite the
composition that's already playing**, so new material is made of the track's
own substance and can't clash. Things *bubble up out of* the track rather than
sit on top of it.

### Signal flow (resolved)

The MP3 lives on the Daisy's **SD card** and plays through the Daisy
**sampler**. `Engine::process` mixes the sampler, then the master runs through
reverb → tape. The Daisy presents itself to the Pi as a **USB audio
interface**, and that UAC source taps the *post-everything master*. So:

- **The finished track flows through the one bus we fully own.** Tape, reverb,
  and any inserted DSP already wrap the whole composition — no line-in, no
  `_input` plumbing, no browser Web Audio graph. The `_input` arg stays unused.
- **Audio and visuals move together for free.** The Pi analyzes the
  *post-processed* master, so any interaction that degrades or blooms the
  sound *is* the FFT signal the visualizer sees. Distance→`TapeFailure` makes
  the track dissolve *and* the dither/slice/glitch picture fall apart in
  lockstep. The audio path is the audiovisual coupling — no extra
  sensor→visual wiring needed.

### Three tiers of "bubbling up" (cleanest first)

1. **Process the finished mix — zero harmonic risk.** Touch the existing audio,
   add no notes. Distance→`TapeFailure` (same composition, dissolving with
   distance / resolving on approach). Presence→reverb & delay wet. Touch→a
   momentary "catch" delay tap that grabs whatever fragment is playing *right
   now* and lets it bubble and repeat — repeats are the track's own material,
   always in key. (Today the ping-pong delay is fed only by the stab send; tap
   the **sampler/master** bus instead, or in addition.)
2. **Excite the track's own resonances — it rings itself.** Feed the track's
   audio through a resonant `Svf` (`svf.rs`) — or a small bank — and open
   Q / sweep cutoff on interaction so the composition's own energy blooms into
   sustained pitched tone. No new material; the track resonates itself.
   **This idea gets *better* with this composition, not worse:** the long
   ambiance is largely **rain / street noise**, and broadband noise → high-Q
   resonant filter is the classic way to bloom pitched tone out of nothing.
   The gesture reads as *tuning into pitched tones hidden in the noise of the
   street* — dead on-concept. Two caveats from the structure: (a) no single
   key over 18 min, so the resonator's tuning should **follow the current
   section** (whichever distant song is up, or a slow ambient center), which
   needs a per-section key lane in the timeline; (b) during a clear distant
   song the source is tonal pads/leads, so the bloom amplifies *its* notes —
   keep the resonant pitches consonant with that song to avoid clashing.

   **Tuning — DECIDED: D Lydian** (`D E F# G# A B C#`) as a single global
   tuning to start (skip the per-section key lane for v1; the piece is "mostly
   D Lydian"). The raised 4th (**G#**) is the Lydian signature — floating,
   far-off, unmoored — which suits "from afar" and the dissociation theme, so
   foreground it as the *reveal*. Starter bank, ~6 `Svf`s in band-pass, fed
   from a pre-tape master tap (freqs at A4=440):

   | Resonator | Note | Hz | Role |
   |---|---|---|---|
   | 1 | D3  | 146.83 | root, low body |
   | 2 | A3  | 220.00 | fifth |
   | 3 | D4  | 293.66 | root octave |
   | 4 | F#4 | 369.99 | major third (color) |
   | 5 | A4  | 440.00 | fifth, presence |
   | 6 | **G#4** | 415.30 | **#4 — Lydian signature, ramps in only in the closest band** |

   Distance → `Svf::set_res` (Q) and a bloom send gain: far = low Q + ~0 send
   (silent), approach = Q toward ~0.9 + send up, so the rain/pads *sing* into a
   Dadd9-with-a-floating-#4 chord as you near. Resonators 1–5 fade in across
   the approach; **6 (G#) only blooms in the final near-band** — lean in far
   enough and the Lydian color surfaces as a payoff. Couple this to idea A's
   clarity-pull (same ToF signal): approaching both *resolves* the mix and
   *blooms* the resonance — one gesture, two coordinated effects.

   Cost: a band-pass `Svf` bank is a handful of multiply-adds per sample and
   needs **no delay buffer**, so unlike the catch-delay it has no SDRAM
   concern — cheap on the embedded target.

   **Prototype status:** implemented in `dsp::bloom::BloomBank`, tapped pre-tape
   in `Engine::process`, driven by `Engine::set_bloom_amount(0..1)`. On the
   `host` an internal LFO sweeps amount far→near→far every 8 bars @ 112 BPM
   (17.143 s) in place of the ToF sensor — `cargo run -p host -- <mp3>`. Tuning
   constants (`RES_MIN/MAX`, `INPUT_ATTEN`, `MASTER_GAIN`, per-voice
   gain/onset/width) live at the top of `bloom.rs`; tune by ear, then the real
   exhibit swaps the LFO for the kiosk distance sensor via CC → a `Param`.
3. **Added voices — only if subordinate and glued.** If the FM stab/kick
   surface at all: lock to the track (key via `prog`/`key:`, tempo already
   locked to the BPM curve); keep sparse and gestural (single hits on
   interaction, never a running pattern); route through the *same* tape +
   reverb the master uses so they read as part of the recording, not a clean
   overdub; duck under the track and have the kick reinforce *existing*
   downbeats (low, felt, presence-gated) rather than start a second pulse.

### Placement note

Insert the interaction processors (catch-delay, SVF bloom) **pre-tape** on the
master. Then `TapeFailure` is a true master "distance" over the *entire*
interactive scene — track plus anything bubbling out of it share one
dying-tape character, so nothing reads as a clean overdub. Tape becomes the
global "how far down the street are you" knob.

(Embedded budget: extra delay/resonator buffers belong in SDRAM, like the
existing reverb/stab-delay buffers — see `[[daisy-fx-buffers-sdram]]`.)
