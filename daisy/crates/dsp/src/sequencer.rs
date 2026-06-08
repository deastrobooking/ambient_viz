//! Step sequencer with per-pattern time resolution (8th / 16th / triplet …).
//!
//! A pattern is a grid of `steps` cells looping over `loop_seconds`. The note
//! value of one step is set by the `res:` header (default 8 = 8th notes): a
//! `res` of N means each step is a 1/Nth note, i.e. `N/4` steps per beat in
//! 4/4. So `res: 16` → 16th notes → 4 steps/beat; `res: 8` → 8ths → 2/beat.
//!
//! Drum voices carry per-step velocity (0.0 = silent):
//! - **kick** — main kick drum (Option<f32> velocity exposed downstream)
//! - **chat** — closed hi-hat (treated as bool downstream: any nonzero = fire)
//! - **ohat** — open hi-hat (ditto)
//!
//! Pitch lives on a separate pair of lanes so the rhythm grid stays readable:
//! - **stab** — a velocity grid (like the drums) saying *when* an FM stab fires.
//! - **prog** — a list of chords saying *what* plays. Each stab trigger pops the
//!   next chord in `prog`, wrapping; `prog` restarts at the top of every loop so
//!   the loop is deterministic. Chords are roman numerals diatonic to `key:`,
//!   absolute chord names, or explicit `[..]` voicings — see [`crate::chord`].
//!
//! Patterns are loaded from `.pat` grid files via [`parse_grid`] +
//! [`Sequencer::load_grid`]. See the file `static/<song>.pat` for an example,
//! and the `parse_grid` doc for the format spec.
//!
//! Sample-accurate beat scheduling is still locked to a tempo curve from
//! [`crate::timeline`] — the sequencer advances a per-sample `step_phase`
//! using the instantaneous BPM, so mid-loop tempo changes adjust the
//! inter-step interval immediately without drift.

use heapless::Vec;

use crate::chord::{self, parse_chord, parse_key, tokenize_prog, Chord, Key};
use crate::timeline::{bpm_at, Keypoint, MAX_KEYPOINTS};

/// Default loop length in steps when a pattern is built in code (4 bars of
/// 8th notes). Patterns loaded from a grid set their own length up to
/// [`MAX_GRID_STEPS`].
pub const STEPS_PER_LOOP: usize = 32;
/// Default steps-per-beat (8th-note resolution in 4/4). Patterns override this
/// via the `res:` header. See [`res_to_steps_per_beat`].
pub const STEPS_PER_BEAT: usize = 2;
/// Default note resolution (8 = 8th notes) when a pattern omits `res:`.
pub const DEFAULT_RES: usize = 8;

/// Max grid file pattern length the parser accepts — and the fixed storage
/// size of every voice array, so any `steps` up to this is supported at any
/// resolution (e.g. 64 sixteenths, or 96 sixteenth-triplets).
pub const MAX_GRID_STEPS: usize = 128;

/// Max chords in a `prog:` progression.
pub const MAX_PROG: usize = 64;

/// Convert a `res:` note division (4, 8, 16, 32, 12 for triplets …) into
/// steps-per-beat in 4/4: a 1/N note means `N/4` steps per quarter-note beat.
/// Returns `None` if `res` isn't a positive multiple of 4 (would not divide a
/// beat into a whole number of steps).
pub fn res_to_steps_per_beat(res: usize) -> Option<usize> {
    if res == 0 || res % 4 != 0 {
        return None;
    }
    Some(res / 4)
}

/// A stab chord fired on this sample.
#[derive(Debug, Clone, Copy, Default)]
pub struct StabHit {
    pub chord: Chord,
    pub velocity: f32,
    /// Optional per-hit "tone" (0..1) from the `stabtone:` lane: low = dark /
    /// short / filtered, high = bright / long / open. `None` (lane absent or a
    /// `.` cell) plays the pristine patch with no filter.
    pub tone: Option<f32>,
}

/// Gate event for the monophonic bass voice. Unlike the fire-and-forget stab,
/// the bass needs an explicit note-off so its envelope can sustain-then-release
/// — the note's *duration* is the number of steps the gate is held open, which
/// (being a step count) stays locked to the tempo curve with zero drift.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BassEvent {
    /// No bass change this sample.
    #[default]
    None,
    /// Strike (or re-strike) a note. `note` is pre-octave-offset MIDI.
    NoteOn { note: u8, vel: f32 },
    /// Release the held note (begin its envelope tail).
    NoteOff,
}

/// One cell of the `bass:` lane. `Hold` (`_`) keeps the gate open across steps
/// without retriggering — this is how note duration is written in the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BassCell {
    /// Gate off (`.`/`-`).
    Rest,
    /// Sustain the current note, no retrigger (`_`).
    Hold,
    /// Strike a new note at this velocity (`X`/`x`/`1`-`9`).
    Strike(f32),
}

impl Default for BassCell {
    fn default() -> Self {
        BassCell::Rest
    }
}

/// One sample's worth of trigger output from [`Sequencer::advance`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StepEvent {
    /// `Some(v)` = trigger the kick at velocity `v`. `None` = no kick.
    pub kick_velocity: Option<f32>,
    pub closed_hat: bool,
    pub open_hat: bool,
    /// `Some(hit)` = trigger an FM stab chord this sample.
    pub stab: Option<StabHit>,
    /// Gate event for the monophonic rumble-bass voice.
    pub bass: BassEvent,
}

pub struct Sequencer {
    sample_rate: f32,
    /// Loop-relative playback time in seconds. Wraps at `loop_seconds`.
    time_seconds: f32,
    /// Length of one audio loop iteration. Set via [`Sequencer::set_tempo`].
    loop_seconds: f32,
    /// Sorted BPM keypoints from the timeline JSON.
    bpm_keypoints: Vec<Keypoint, MAX_KEYPOINTS>,
    /// Fractional position within the current *step*, [0, 1). Initialised to
    /// 1.0 so the very first sample fires step 0.
    step_phase: f32,
    /// Which step of the pattern fires next.
    step: u32,
    /// Active loop length in steps (≤ [`MAX_GRID_STEPS`]).
    steps_per_loop: usize,
    /// Active steps-per-beat (timing resolution). 2 = 8ths, 4 = 16ths, …
    steps_per_beat: usize,
    /// Per-voice velocity arrays. Sized to the max; only `steps_per_loop` of
    /// each is meaningful.
    kick_pattern: [f32; MAX_GRID_STEPS],
    chat_pattern: [f32; MAX_GRID_STEPS],
    ohat_pattern: [f32; MAX_GRID_STEPS],
    /// Per-step velocity for the stab lane (when a chord fires).
    stab_pattern: [f32; MAX_GRID_STEPS],
    /// Per-step stab "tone" (0..1), or `< 0` for "none" (pristine). Sentinel
    /// avoids an `Option` array so it copies like the other lanes.
    stabtone_pattern: [f32; MAX_GRID_STEPS],
    /// Per-step bass-lane cells (strike / hold / rest).
    bass_pattern: [BassCell; MAX_GRID_STEPS],
    /// Whether the bass gate is currently open (a note is sounding/sustaining).
    bass_gate_open: bool,
    /// Key context for resolving roman-numeral chords.
    key: Key,
    /// Base octave for named/roman chords (notes without their own octave).
    base_octave: i32,
    /// The chord progression, consumed one per stab hit (wraps).
    prog: Vec<Chord, MAX_PROG>,
    /// Cursor into `prog`; advances per stab hit, resets each loop.
    prog_cursor: usize,
    /// The bass progression (lowest note of each chord is played). Consumed one
    /// per bass *strike* (wraps); restarts each loop alongside `prog`.
    bassprog: Vec<Chord, MAX_PROG>,
    /// Cursor into `bassprog`; advances per bass strike, resets each loop.
    bassprog_cursor: usize,
    /// If false, [`Sequencer::advance`] always returns a default (empty) event.
    enabled: bool,

    // Lifetime trigger counters (debug — host polls & diffs for firing rate).
    kick_count: u64,
    closed_hat_count: u64,
    open_hat_count: u64,
    stab_count: u64,
    bass_count: u64,
}

/// Built-in default: matches the user's pre-grid hand-coded sequence.
/// Bar 1 = `X . x . X . x .` for kick, with closed hats on beats + odd
/// upbeats and open hats on even upbeats. Pattern repeats for all 4 bars.
pub const DEFAULT_KICK: [f32; STEPS_PER_LOOP] = [
    1.0, 0.0, 0.7, 0.0, 1.0, 0.0, 0.7, 0.0, // bar 1
    1.0, 0.0, 0.7, 0.0, 1.0, 0.0, 0.7, 0.0, // bar 2
    1.0, 0.0, 0.7, 0.0, 1.0, 0.0, 0.7, 0.0, // bar 3
    1.0, 0.0, 0.7, 0.0, 1.0, 0.0, 0.7, 0.0, // bar 4
];
pub const DEFAULT_CHAT: [f32; STEPS_PER_LOOP] = [
    1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0,
    0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0,
];
pub const DEFAULT_OHAT: [f32; STEPS_PER_LOOP] = [
    0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
    1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0,
];

/// Copy a short default pattern into a full-size storage array (rest silent).
fn widen(src: &[f32; STEPS_PER_LOOP]) -> [f32; MAX_GRID_STEPS] {
    let mut out = [0.0; MAX_GRID_STEPS];
    out[..STEPS_PER_LOOP].copy_from_slice(src);
    out
}

impl Sequencer {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            time_seconds: 0.0,
            loop_seconds: 0.0,
            bpm_keypoints: Vec::new(),
            step_phase: 1.0,
            step: 0,
            steps_per_loop: STEPS_PER_LOOP,
            steps_per_beat: STEPS_PER_BEAT,
            kick_pattern: widen(&DEFAULT_KICK),
            chat_pattern: widen(&DEFAULT_CHAT),
            ohat_pattern: widen(&DEFAULT_OHAT),
            stab_pattern: [0.0; MAX_GRID_STEPS], // stabs opt in via stab:/prog:
            stabtone_pattern: [-1.0; MAX_GRID_STEPS], // -1 = none → pristine pluck
            bass_pattern: [BassCell::Rest; MAX_GRID_STEPS], // bass opts in via bass:/bassprog:
            bass_gate_open: false,
            key: Key::default(),
            base_octave: chord::DEFAULT_OCTAVE,
            prog: Vec::new(),
            prog_cursor: 0,
            bassprog: Vec::new(),
            bassprog_cursor: 0,
            enabled: false,
            kick_count: 0,
            closed_hat_count: 0,
            open_hat_count: 0,
            stab_count: 0,
            bass_count: 0,
        }
    }

    pub fn kick_count(&self) -> u64 {
        self.kick_count
    }
    pub fn closed_hat_count(&self) -> u64 {
        self.closed_hat_count
    }
    pub fn open_hat_count(&self) -> u64 {
        self.open_hat_count
    }
    pub fn stab_count(&self) -> u64 {
        self.stab_count
    }
    /// Number of bass *strikes* fired (note-ons, not holds).
    pub fn bass_count(&self) -> u64 {
        self.bass_count
    }
    /// Active loop length in steps.
    pub fn steps_per_loop(&self) -> usize {
        self.steps_per_loop
    }
    /// Active timing resolution in steps-per-beat (2 = 8ths, 4 = 16ths).
    pub fn steps_per_beat(&self) -> usize {
        self.steps_per_beat
    }

    /// Configure the tempo curve and loop length. Enables the sequencer.
    pub fn set_tempo(&mut self, keypoints: Vec<Keypoint, MAX_KEYPOINTS>, loop_seconds: f32) {
        self.bpm_keypoints = keypoints;
        self.loop_seconds = loop_seconds;
        self.enabled = true;
    }

    /// Set the timing resolution directly (steps-per-beat: 2 = 8ths, 4 = 16ths).
    /// Clamped to ≥ 1. Loaded patterns set this from their `res:` header.
    pub fn set_steps_per_beat(&mut self, spb: usize) {
        self.steps_per_beat = spb.max(1);
    }

    pub fn set_kick_pattern(&mut self, pattern: &[f32]) {
        self.set_pattern(Voice::Kick, pattern);
    }
    pub fn set_chat_pattern(&mut self, pattern: &[f32]) {
        self.set_pattern(Voice::Chat, pattern);
    }
    pub fn set_ohat_pattern(&mut self, pattern: &[f32]) {
        self.set_pattern(Voice::Ohat, pattern);
    }
    pub fn set_stab_pattern(&mut self, pattern: &[f32]) {
        self.set_pattern(Voice::Stab, pattern);
    }

    /// Replace one voice's pattern and adopt its length as the loop length.
    /// Length is clamped to [`MAX_GRID_STEPS`]. The other voices keep their
    /// cells; cells beyond the new length stop playing.
    fn set_pattern(&mut self, voice: Voice, pattern: &[f32]) {
        let n = pattern.len().min(MAX_GRID_STEPS);
        let arr = self.voice_array(voice);
        for (dst, src) in arr[..n].iter_mut().zip(pattern.iter()) {
            *dst = src.clamp(0.0, 1.0);
        }
        if n > 0 {
            self.steps_per_loop = n;
        }
    }

    fn voice_array(&mut self, voice: Voice) -> &mut [f32; MAX_GRID_STEPS] {
        match voice {
            Voice::Kick => &mut self.kick_pattern,
            Voice::Chat => &mut self.chat_pattern,
            Voice::Ohat => &mut self.ohat_pattern,
            Voice::Stab => &mut self.stab_pattern,
        }
    }

    /// Set the key + base octave used to resolve roman-numeral chords.
    pub fn set_key(&mut self, key: Key, base_octave: i32) {
        self.key = key;
        self.base_octave = base_octave;
    }

    /// Replace the chord progression directly (bypassing the `.pat` parser).
    pub fn set_prog(&mut self, chords: &[Chord]) {
        self.prog.clear();
        for &c in chords {
            if self.prog.push(c).is_err() {
                break;
            }
        }
        self.prog_cursor = 0;
    }

    pub fn set_step_velocity(&mut self, voice: Voice, idx: usize, velocity: f32) {
        if idx >= self.steps_per_loop {
            return;
        }
        let v = velocity.clamp(0.0, 1.0);
        self.voice_array(voice)[idx] = v;
    }

    pub fn step_velocity(&self, voice: Voice, idx: usize) -> Option<f32> {
        if idx >= self.steps_per_loop {
            return None;
        }
        Some(match voice {
            Voice::Kick => self.kick_pattern[idx],
            Voice::Chat => self.chat_pattern[idx],
            Voice::Ohat => self.ohat_pattern[idx],
            Voice::Stab => self.stab_pattern[idx],
        })
    }

    pub fn set_bass_step(&mut self, idx: usize, cell: BassCell) {
        if idx >= self.steps_per_loop {
            return;
        }
        self.bass_pattern[idx] = cell;
    }

    pub fn bass_step(&self, idx: usize) -> Option<BassCell> {
        if idx >= self.steps_per_loop {
            return None;
        }
        Some(self.bass_pattern[idx])
    }

    /// Parse and apply a grid file in one step. See [`parse_grid`] for format.
    pub fn load_grid(&mut self, text: &str) -> Result<PatternGrid, ParseError> {
        let grid = parse_grid(text)?;
        if grid.steps == 0 || grid.steps > MAX_GRID_STEPS {
            return Err(ParseError::WrongStepCount {
                expected: MAX_GRID_STEPS,
                got: grid.steps,
            });
        }
        // The parser pads every voice to `steps`, so all four are that length.
        if grid.kick.len() != grid.steps
            || grid.chat.len() != grid.steps
            || grid.ohat.len() != grid.steps
            || grid.stab.len() != grid.steps
        {
            return Err(ParseError::VoiceLengthMismatch);
        }

        // Resolution from `res:` (default 8 = 8ths). Must divide a beat evenly.
        let res = grid.res.unwrap_or(DEFAULT_RES);
        self.steps_per_beat = res_to_steps_per_beat(res).ok_or(ParseError::BadRes)?;
        self.steps_per_loop = grid.steps;

        // Copy the active region; zero the tail so stale cells never fire.
        self.kick_pattern = [0.0; MAX_GRID_STEPS];
        self.chat_pattern = [0.0; MAX_GRID_STEPS];
        self.ohat_pattern = [0.0; MAX_GRID_STEPS];
        self.stab_pattern = [0.0; MAX_GRID_STEPS];
        self.stabtone_pattern = [-1.0; MAX_GRID_STEPS]; // -1 = none (pristine)
        self.bass_pattern = [BassCell::Rest; MAX_GRID_STEPS];
        self.kick_pattern[..grid.steps].copy_from_slice(&grid.kick);
        self.chat_pattern[..grid.steps].copy_from_slice(&grid.chat);
        self.ohat_pattern[..grid.steps].copy_from_slice(&grid.ohat);
        self.stab_pattern[..grid.steps].copy_from_slice(&grid.stab);
        self.stabtone_pattern[..grid.steps].copy_from_slice(&grid.stabtone);
        self.bass_pattern[..grid.steps].copy_from_slice(&grid.bass);

        // Harmony: key (default C minor if absent), base octave, then resolve
        // each progression token into a chord in that key.
        self.key = if grid.key.is_empty() {
            Key::default()
        } else {
            parse_key(&grid.key).ok_or(ParseError::BadKey)?
        };
        self.base_octave = grid.octave.unwrap_or(chord::DEFAULT_OCTAVE);
        self.prog.clear();
        for tok in grid.prog.iter() {
            if let Some(c) = parse_chord(tok, &self.key, self.base_octave) {
                let _ = self.prog.push(c);
            }
        }
        self.prog_cursor = 0;
        // Bass progression: same chord syntax, lowest note played per strike.
        // Falls back to `prog` if no dedicated `bassprog:` was given, so a
        // bass lane can ride the stab harmony without restating it.
        self.bassprog.clear();
        let bass_src = if grid.bassprog.is_empty() {
            &grid.prog
        } else {
            &grid.bassprog
        };
        for tok in bass_src.iter() {
            if let Some(c) = parse_chord(tok, &self.key, self.base_octave) {
                let _ = self.bassprog.push(c);
            }
        }
        self.bassprog_cursor = 0;
        self.bass_gate_open = false;
        // Restart cleanly on the new grid.
        self.step = 0;
        self.step_phase = 1.0;

        Ok(grid)
    }

    pub fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn step(&self) -> u32 {
        self.step
    }
    pub fn time_seconds(&self) -> f32 {
        self.time_seconds
    }

    /// Reset playback position to the loop start.
    pub fn reset(&mut self) {
        self.time_seconds = 0.0;
        self.step = 0;
        self.step_phase = 1.0;
        self.prog_cursor = 0;
        self.bassprog_cursor = 0;
        self.bass_gate_open = false;
    }

    /// Advance one audio sample. Returns a [`StepEvent`] describing any
    /// triggers that fire on this sample. Call exactly once per output sample.
    pub fn advance(&mut self) -> StepEvent {
        let mut evt = StepEvent::default();
        if !self.enabled {
            return evt;
        }

        self.time_seconds += 1.0 / self.sample_rate;
        if self.loop_seconds > 0.0 && self.time_seconds >= self.loop_seconds {
            self.time_seconds -= self.loop_seconds;
            self.step = 0;
            self.step_phase = 1.0;
            // Restart the progressions so every loop iteration is identical.
            // NOTE: we deliberately do *not* touch `bass_gate_open` here — a
            // bass note whose hold cells run to the end of the loop keeps its
            // gate open across the wrap, so sustain is seamless between loops
            // (the next loop's first cell decides whether to re-strike/release).
            self.prog_cursor = 0;
            self.bassprog_cursor = 0;
        }

        let bpm = bpm_at(&self.bpm_keypoints, self.time_seconds);
        // Steps per second = beats/sec × steps/beat. Bumping steps_per_beat
        // (e.g. 2 → 4 for 16ths) doubles the rate, no other code changes.
        let step_rate = (bpm / 60.0) * self.steps_per_beat as f32;
        self.step_phase += step_rate / self.sample_rate;

        if self.step_phase >= 1.0 {
            self.step_phase -= 1.0;
            let idx = self.step as usize;
            let kv = self.kick_pattern[idx];
            let cv = self.chat_pattern[idx];
            let ov = self.ohat_pattern[idx];
            let sv = self.stab_pattern[idx];

            if kv > 0.0 {
                evt.kick_velocity = Some(kv);
                self.kick_count += 1;
            }
            if cv > 0.0 {
                evt.closed_hat = true;
                self.closed_hat_count += 1;
            }
            if ov > 0.0 {
                evt.open_hat = true;
                self.open_hat_count += 1;
            }
            if sv > 0.0 && !self.prog.is_empty() {
                let chord = self.prog[self.prog_cursor % self.prog.len()];
                let tv = self.stabtone_pattern[idx];
                evt.stab = Some(StabHit {
                    chord,
                    velocity: sv,
                    tone: if tv >= 0.0 { Some(tv) } else { None },
                });
                self.prog_cursor = self.prog_cursor.wrapping_add(1);
                self.stab_count += 1;
            }

            // Bass lane → gated note events for the monophonic voice.
            match self.bass_pattern[idx] {
                BassCell::Strike(vel) => {
                    // Lowest note of the next bassprog chord (mono sub-bass).
                    if let Some(note) = self
                        .bassprog
                        .get(self.bassprog_cursor % self.bassprog.len().max(1))
                        .and_then(|c| c.notes().iter().min().copied())
                    {
                        evt.bass = BassEvent::NoteOn { note, vel };
                        self.bass_gate_open = true;
                        self.bass_count += 1;
                    }
                    // Advance the cursor on every strike, even if bassprog is
                    // empty (no-op via the max(1) guard above → get None).
                    self.bassprog_cursor = self.bassprog_cursor.wrapping_add(1);
                }
                BassCell::Hold => {
                    // Sustain: keep the gate as-is, emit nothing.
                }
                BassCell::Rest => {
                    if self.bass_gate_open {
                        evt.bass = BassEvent::NoteOff;
                        self.bass_gate_open = false;
                    }
                }
            }

            self.step = (self.step + 1) % self.steps_per_loop as u32;
        }

        evt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    Kick,
    Chat,
    Ohat,
    Stab,
}

// ---------------------------------------------------------------------------
// Grid file format
// ---------------------------------------------------------------------------

/// Parsed `.pat` grid file. Use [`Sequencer::load_grid`] to apply it.
pub struct PatternGrid {
    pub name: heapless::String<64>,
    pub steps: usize,
    /// Note resolution from `res:` (4/8/16/…); `None` if absent (→ default 8).
    pub res: Option<usize>,
    pub kick: Vec<f32, MAX_GRID_STEPS>,
    pub chat: Vec<f32, MAX_GRID_STEPS>,
    pub ohat: Vec<f32, MAX_GRID_STEPS>,
    pub stab: Vec<f32, MAX_GRID_STEPS>,
    /// Per-step stab tone (0..1), `-1` = none. Empty if no `stabtone:` row.
    pub stabtone: Vec<f32, MAX_GRID_STEPS>,
    /// Bass lane cells (strike / hold / rest). Empty if no `bass:` row.
    pub bass: Vec<BassCell, MAX_GRID_STEPS>,
    /// Raw `key:` value (e.g. "C minor"); empty if absent.
    pub key: heapless::String<32>,
    /// `octave:` header — base octave for named/roman chords.
    pub octave: Option<i32>,
    /// Raw `prog:` chord tokens, in order. Resolved against the key on load.
    pub prog: Vec<heapless::String<24>, MAX_PROG>,
    /// Raw `bassprog:` chord tokens. If empty, the bass rides `prog`.
    pub bassprog: Vec<heapless::String<24>, MAX_PROG>,
}

#[derive(Debug, Clone, Copy)]
pub enum ParseError {
    /// `steps:` header missing or unparseable.
    BadHeader,
    /// `key:` value couldn't be parsed (bad root note or unknown mode).
    BadKey,
    /// `res:` value isn't a positive multiple of 4.
    BadRes,
    /// One of the voice rows has more cells than [`MAX_GRID_STEPS`].
    TooManyCells,
    /// At least one voice has a cell count != the declared `steps:`.
    VoiceLengthMismatch,
    /// Declared `steps:` is 0 or exceeds [`MAX_GRID_STEPS`].
    WrongStepCount { expected: usize, got: usize },
}

/// Parse a `.pat` grid file.
///
/// **Format:**
/// - Lines starting with `#` are comments. Blank lines are ignored.
/// - Header keys: `name: <str>` (optional), `steps: <usize>` (required),
///   `res: <div>` (optional, default 8 — note division: 8 = 8ths, 16 = 16ths,
///   must be a multiple of 4), `key: <root> <mode>` (optional, default
///   `C minor`), `octave: <int>` (optional, default 3 — base octave for chords).
/// - Drum/stab rows: `kick:`, `chat:`, `ohat:`, `stab:` — cell sequences:
///   - `X` = full velocity (1.0)
///   - `x` = soft velocity (0.7)
///   - `.` or `-` = silent (0.0)
///   - `0`..`9` = 0%, 11%, … 100% (digit / 9)
///   - Whitespace, `|`, and `,` are ignored (use them for visual grouping).
///   - Any other character is ignored.
/// - Harmony row: `prog: <chords>` — an ordered list of chord tokens, one
///   consumed per `stab:` hit (wraps each loop). Tokens are roman numerals
///   (`i iv V`), chord names (`Cm Ab Ebmaj7`), or `[..]` voicings. `.`/`-`
///   are visual filler; `|` and `,` group. See [`crate::chord`].
/// - Stab tone lane: `stabtone:` — one digit `0`-`9` per step setting that
///   hit's character on a single co-varying axis (0 = dark / short / heavily
///   filtered, 9 = bright / long / open). `.`/`-` = "none" → the pristine
///   patch with the per-voice filter bypassed. Absent row = all pristine.
/// - Bass lane: `bass:` — like the drum rows, but with a **tie char** for
///   duration on the monophonic sub-bass:
///   - `X`/`x`/`1`-`9` = strike a new note (velocity as for drums)
///   - `_` = hold (sustain the current note across this step, no retrigger)
///   - `.`/`-` = rest (release the held note)
///   A held note that runs to the loop end sustains seamlessly into the next
///   loop. `bassprog: <chords>` supplies the pitches (lowest note of each chord
///   is played, one per strike, wrapping); if omitted, the bass rides `prog`.
///
/// Present voice rows must each have exactly `steps:` cells. Absent rows are
/// left silent.
pub fn parse_grid(text: &str) -> Result<PatternGrid, ParseError> {
    let mut grid = PatternGrid {
        name: heapless::String::new(),
        steps: 0,
        res: None,
        kick: Vec::new(),
        chat: Vec::new(),
        ohat: Vec::new(),
        stab: Vec::new(),
        stabtone: Vec::new(),
        bass: Vec::new(),
        key: heapless::String::new(),
        octave: None,
        prog: Vec::new(),
        bassprog: Vec::new(),
    };
    let mut got_steps = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("name:") {
            let _ = grid.name.push_str(rest.trim());
            continue;
        }
        if let Some(rest) = line.strip_prefix("steps:") {
            grid.steps = rest.trim().parse().map_err(|_| ParseError::BadHeader)?;
            got_steps = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("res:") {
            grid.res = Some(rest.trim().parse().map_err(|_| ParseError::BadRes)?);
            continue;
        }
        if let Some(rest) = line.strip_prefix("key:") {
            let _ = grid.key.push_str(rest.trim());
            continue;
        }
        if let Some(rest) = line.strip_prefix("octave:") {
            grid.octave = Some(rest.trim().parse().map_err(|_| ParseError::BadHeader)?);
            continue;
        }
        // `bassprog:` must be tested before `prog:` so the longer key wins.
        if let Some(rest) = line.strip_prefix("bassprog:") {
            grid.bassprog = tokenize_prog::<MAX_PROG>(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("prog:") {
            grid.prog = tokenize_prog::<MAX_PROG>(rest);
            continue;
        }

        // Bass lane: tie-char cells (strike / hold / rest), parsed separately
        // from the drum/stab velocity rows below.
        if let Some(rest) = line.strip_prefix("bass:") {
            for ch in rest.chars() {
                let cell = match ch {
                    'X' => BassCell::Strike(1.0),
                    'x' => BassCell::Strike(0.7),
                    '0'..='9' => BassCell::Strike((ch as u8 - b'0') as f32 / 9.0),
                    '_' => BassCell::Hold,
                    '.' | '-' => BassCell::Rest,
                    ' ' | '\t' | '|' | ',' => continue,
                    _ => continue,
                };
                grid.bass.push(cell).map_err(|_| ParseError::TooManyCells)?;
            }
            continue;
        }

        // Stab tone lane: digit 0-9 → tone 0..1; `.`/`-` → none (sentinel -1).
        // Tested before the generic `stab:` row so the longer key wins.
        if let Some(rest) = line.strip_prefix("stabtone:") {
            for ch in rest.chars() {
                let v = match ch {
                    '0'..='9' => (ch as u8 - b'0') as f32 / 9.0,
                    '.' | '-' => -1.0,
                    ' ' | '\t' | '|' | ',' => continue,
                    _ => continue,
                };
                grid.stabtone
                    .push(v)
                    .map_err(|_| ParseError::TooManyCells)?;
            }
            continue;
        }

        // Drum/stab voice rows.
        let (rest, target) = if let Some(rest) = line.strip_prefix("kick:") {
            (rest, &mut grid.kick)
        } else if let Some(rest) = line.strip_prefix("chat:") {
            (rest, &mut grid.chat)
        } else if let Some(rest) = line.strip_prefix("ohat:") {
            (rest, &mut grid.ohat)
        } else if let Some(rest) = line.strip_prefix("stab:") {
            (rest, &mut grid.stab)
        } else {
            continue; // Unknown row — silently skip so future voices don't break old files.
        };

        for ch in rest.chars() {
            let vel = match ch {
                'X' => 1.0,
                'x' => 0.7,
                '.' | '-' => 0.0,
                ' ' | '\t' | '|' | ',' => continue,
                '0'..='9' => ((ch as u8 - b'0') as f32) / 9.0,
                _ => continue,
            };
            target.push(vel).map_err(|_| ParseError::TooManyCells)?;
        }
    }

    if !got_steps {
        return Err(ParseError::BadHeader);
    }
    // Allow a voice to be entirely absent (treated as silent). Only enforce
    // length when the voice is present.
    let any_voice_mismatch = (!grid.kick.is_empty() && grid.kick.len() != grid.steps)
        || (!grid.chat.is_empty() && grid.chat.len() != grid.steps)
        || (!grid.ohat.is_empty() && grid.ohat.len() != grid.steps)
        || (!grid.stab.is_empty() && grid.stab.len() != grid.steps)
        || (!grid.stabtone.is_empty() && grid.stabtone.len() != grid.steps)
        || (!grid.bass.is_empty() && grid.bass.len() != grid.steps);
    if any_voice_mismatch {
        return Err(ParseError::VoiceLengthMismatch);
    }
    // Pad absent voices with silence so callers don't have to special-case them.
    pad_silent(&mut grid.kick, grid.steps);
    pad_silent(&mut grid.chat, grid.steps);
    pad_silent(&mut grid.ohat, grid.steps);
    pad_silent(&mut grid.stab, grid.steps);
    // Stab-tone pads with the "none" sentinel (-1), not silence.
    while grid.stabtone.len() < grid.steps {
        if grid.stabtone.push(-1.0).is_err() {
            break;
        }
    }
    while grid.bass.len() < grid.steps {
        if grid.bass.push(BassCell::Rest).is_err() {
            break;
        }
    }

    Ok(grid)
}

fn pad_silent(v: &mut Vec<f32, MAX_GRID_STEPS>, steps: usize) {
    while v.len() < steps {
        if v.push(0.0).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAT: &str = "\
name: stab test
steps: 32
key: C minor
octave: 3
kick: X . . . X . . . X . . . X . . . X . . . X . . . X . . . X . . .
stab: X . . . X . . . X . . . X . . . . . . . . . . . . . . . . . . .
prog: i iv VI v
";

    // 64-step, 16th-note pattern: kick on every quarter (steps 0,4,8,…),
    // hats on every 16th. Exercises res: + a non-default loop length.
    const PAT16: &str = "\
name: sixteenths
steps: 64
res: 16
kick: X...X...X...X...X...X...X...X...X...X...X...X...X...X...X...X...
chat: XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
";

    #[test]
    fn parses_key_octave_and_prog() {
        let grid = parse_grid(PAT).unwrap();
        assert_eq!(grid.steps, 32);
        assert_eq!(grid.res, None);
        assert_eq!(grid.key.as_str(), "C minor");
        assert_eq!(grid.octave, Some(3));
        assert_eq!(grid.prog.len(), 4);
        assert_eq!(grid.stab.len(), 32);
    }

    #[test]
    fn res_maps_to_steps_per_beat() {
        assert_eq!(res_to_steps_per_beat(4), Some(1)); // quarters
        assert_eq!(res_to_steps_per_beat(8), Some(2)); // eighths
        assert_eq!(res_to_steps_per_beat(16), Some(4)); // sixteenths
        assert_eq!(res_to_steps_per_beat(32), Some(8));
        assert_eq!(res_to_steps_per_beat(6), None); // not a multiple of 4
        assert_eq!(res_to_steps_per_beat(0), None);
    }

    #[test]
    fn default_pattern_is_8th_note_resolution() {
        let seq = Sequencer::new(48_000.0);
        assert_eq!(seq.steps_per_beat(), 2);
        assert_eq!(seq.steps_per_loop(), 32);
    }

    #[test]
    fn sixteenth_grid_loads_and_doubles_step_rate() {
        let mut seq = Sequencer::new(48_000.0);
        seq.load_grid(PAT16).unwrap();
        assert_eq!(seq.steps_per_loop(), 64);
        assert_eq!(seq.steps_per_beat(), 4); // 16ths

        // At 120 BPM (2 beats/s), 16ths fire at 8 steps/s. The chat row hits
        // every step, so over 1 s of audio we expect ~8 closed-hat triggers.
        let mut kps: Vec<Keypoint, MAX_KEYPOINTS> = Vec::new();
        let _ = kps.push(Keypoint { t: 0.0, v: 120.0 });
        seq.set_tempo(kps, 8.0); // 64 sixteenths at 120 BPM = 8 s loop
        let mut hats = 0u64;
        for _ in 0..48_000 {
            if seq.advance().closed_hat {
                hats += 1;
            }
        }
        // 8 steps/s ± a step for phase alignment.
        assert!(
            (7..=9).contains(&hats),
            "expected ~8 hats in 1 s, got {hats}"
        );
    }

    #[test]
    fn bad_res_is_rejected() {
        let bad = "steps: 16\nres: 6\nkick: X...X...X...X...\n";
        assert!(matches!(seq_load(bad), Err(ParseError::BadRes)));
    }

    fn seq_load(text: &str) -> Result<PatternGrid, ParseError> {
        let mut seq = Sequencer::new(48_000.0);
        seq.load_grid(text)
    }

    // 16-step bass: a whole-note (strike + 7 holds) then a half + rests, with
    // a held note running to the loop end (seamless-sustain case).
    const PATBASS: &str = "\
name: bass test
steps: 16
res: 16
key: C minor
kick: X . . . X . . . X . . . X . . .
bass: X _ _ _ _ _ _ _ X _ _ _ _ _ _ _
bassprog: i v
";

    fn fixed_tempo(bpm: f32, loop_s: f32) -> Vec<Keypoint, MAX_KEYPOINTS> {
        let mut kps: Vec<Keypoint, MAX_KEYPOINTS> = Vec::new();
        let _ = kps.push(Keypoint { t: 0.0, v: bpm });
        kps
    }

    #[test]
    fn bass_lane_parses_strike_hold_rest() {
        let grid = parse_grid(PATBASS).unwrap();
        assert_eq!(grid.bass.len(), 16);
        assert_eq!(grid.bass[0], BassCell::Strike(1.0));
        assert_eq!(grid.bass[1], BassCell::Hold);
        assert_eq!(grid.bass[8], BassCell::Strike(1.0));
        assert_eq!(grid.bassprog.len(), 2);
    }

    #[test]
    fn held_bass_emits_one_noteon_per_strike_and_plays_chord_root() {
        let mut seq = Sequencer::new(48_000.0);
        seq.load_grid(PATBASS).unwrap();
        // 16 sixteenths at 120 BPM = 2 s loop.
        seq.set_tempo(fixed_tempo(120.0, 2.0), 2.0);
        let (mut ons, mut offs) = (0u32, 0u32);
        let mut first_note = None;
        // Sample just under one full loop (1.9 s of a 2 s loop) so we don't
        // land on the wrap and re-fire step 0.
        for _ in 0..((48_000.0 * 1.9) as usize) {
            match seq.advance().bass {
                BassEvent::NoteOn { note, .. } => {
                    if first_note.is_none() {
                        first_note = Some(note);
                    }
                    ons += 1;
                }
                BassEvent::NoteOff => offs += 1,
                BassEvent::None => {}
            }
        }
        // Two strikes per loop (steps 0 and 8), no rests → no note-offs in the
        // loop body; the gate stays open across the wrap (seamless sustain).
        assert_eq!(ons, 2, "two strikes per loop");
        assert_eq!(offs, 0, "no rests → no note-off; sustain crosses the loop");
        // `i` in C minor at default octave 3 = C(48) Eb(51) G(55); bass plays
        // the lowest note → 48, then offset -12 happens in the voice, not here.
        assert_eq!(first_note, Some(48));
    }

    #[test]
    fn bass_note_length_is_tempo_invariant_in_steps() {
        // The whole point: a held note's duration measured in STEPS is the same
        // regardless of tempo (only its wall-clock seconds differ). Count the
        // steps the gate stays open by sampling step() at note-on/off.
        fn steps_held(bpm: f32) -> u32 {
            // One strike, held to the end of a 16-step loop, then a rest pattern
            // that releases at step 12.
            let pat =
                "steps: 16\nres: 16\nkick: X...X...X...X...\nbass: X___________....\nbassprog: i\n";
            let mut seq = Sequencer::new(48_000.0);
            seq.load_grid(pat).unwrap();
            // loop seconds = 16 sixteenths = 4 beats = 4 * 60/bpm.
            let loop_s = 4.0 * 60.0 / bpm;
            seq.set_tempo(fixed_tempo(bpm, loop_s), loop_s);
            let mut on_step = None;
            let mut off_step = None;
            // Run ~1.1 loops so we catch the release.
            let n = (loop_s * 1.1 * 48_000.0) as usize;
            for _ in 0..n {
                let st = seq.step();
                match seq.advance().bass {
                    BassEvent::NoteOn { .. } if on_step.is_none() => on_step = Some(st),
                    BassEvent::NoteOff if off_step.is_none() => off_step = Some(st),
                    _ => {}
                }
            }
            // Release happens at step 12 (first '.' after the 12 strike/holds).
            off_step.unwrap().wrapping_sub(on_step.unwrap())
        }
        let slow = steps_held(60.0);
        let fast = steps_held(140.0);
        assert_eq!(
            slow, fast,
            "held duration in steps must not depend on tempo"
        );
        assert_eq!(slow, 12, "strike at 0, release at 12 → 12 steps held");
    }

    #[test]
    fn load_grid_resolves_chords_and_fires_stabs() {
        let mut seq = Sequencer::new(48_000.0);
        seq.load_grid(PAT).unwrap();
        // Drive a couple of bars at a fixed tempo and confirm stabs fire and
        // cycle the progression.
        let mut kps: Vec<Keypoint, MAX_KEYPOINTS> = Vec::new();
        let _ = kps.push(Keypoint { t: 0.0, v: 120.0 });
        seq.set_tempo(kps, 4.0);
        let mut chords_seen = alloc::vec::Vec::new();
        for _ in 0..(48_000 * 2) {
            if let Some(hit) = seq.advance().stab {
                chords_seen.push(hit.chord);
            }
        }
        assert!(seq.stab_count() >= 2, "stabs should have fired");
        // First fired chord is `i` in C minor = C Eb G at octave 3 = 48,51,55.
        assert_eq!(chords_seen[0].notes(), &[48, 51, 55]);
    }

    const PATTONE: &str = "\
name: tone test
steps: 16
res: 16
key: C minor
stab:     X . . . X . . . X . . . X . . .
stabtone: 2 . . . 9 . . . . . . . 5 . . .
prog: i i i i
";

    #[test]
    fn stabtone_lane_attaches_per_hit_tone() {
        let grid = parse_grid(PATTONE).unwrap();
        assert_eq!(grid.stabtone.len(), 16);
        assert!((grid.stabtone[0] - 2.0 / 9.0).abs() < 1e-6);
        assert!((grid.stabtone[4] - 1.0).abs() < 1e-6); // 9/9
        assert_eq!(grid.stabtone[8], -1.0); // '.' → none
        assert!((grid.stabtone[12] - 5.0 / 9.0).abs() < 1e-6);

        let mut seq = Sequencer::new(48_000.0);
        seq.load_grid(PATTONE).unwrap();
        seq.set_tempo(fixed_tempo(120.0, 2.0), 2.0);
        let mut tones = alloc::vec::Vec::new();
        for _ in 0..((48_000.0 * 1.9) as usize) {
            if let Some(hit) = seq.advance().stab {
                tones.push(hit.tone);
            }
        }
        // Four stab hits: tone 2/9, 9/9, none, 5/9.
        assert_eq!(tones.len(), 4);
        assert!((tones[0].unwrap() - 2.0 / 9.0).abs() < 1e-6);
        assert!((tones[1].unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(tones[2], None);
        assert!((tones[3].unwrap() - 5.0 / 9.0).abs() < 1e-6);
    }
}
