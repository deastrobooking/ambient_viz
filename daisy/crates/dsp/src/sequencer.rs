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

use crate::chord::{self, Chord, Key, parse_chord, parse_key, tokenize_prog};
use crate::timeline::{Keypoint, MAX_KEYPOINTS, bpm_at};

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
    /// Key context for resolving roman-numeral chords.
    key: Key,
    /// Base octave for named/roman chords (notes without their own octave).
    base_octave: i32,
    /// The chord progression, consumed one per stab hit (wraps).
    prog: Vec<Chord, MAX_PROG>,
    /// Cursor into `prog`; advances per stab hit, resets each loop.
    prog_cursor: usize,
    /// If false, [`Sequencer::advance`] always returns a default (empty) event.
    enabled: bool,

    // Lifetime trigger counters (debug — host polls & diffs for firing rate).
    kick_count: u64,
    closed_hat_count: u64,
    open_hat_count: u64,
    stab_count: u64,
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
            key: Key::default(),
            base_octave: chord::DEFAULT_OCTAVE,
            prog: Vec::new(),
            prog_cursor: 0,
            enabled: false,
            kick_count: 0,
            closed_hat_count: 0,
            open_hat_count: 0,
            stab_count: 0,
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
        self.kick_pattern[..grid.steps].copy_from_slice(&grid.kick);
        self.chat_pattern[..grid.steps].copy_from_slice(&grid.chat);
        self.ohat_pattern[..grid.steps].copy_from_slice(&grid.ohat);
        self.stab_pattern[..grid.steps].copy_from_slice(&grid.stab);

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
            // Restart the progression so every loop iteration is identical.
            self.prog_cursor = 0;
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
                evt.stab = Some(StabHit {
                    chord,
                    velocity: sv,
                });
                self.prog_cursor = self.prog_cursor.wrapping_add(1);
                self.stab_count += 1;
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
    /// Raw `key:` value (e.g. "C minor"); empty if absent.
    pub key: heapless::String<32>,
    /// `octave:` header — base octave for named/roman chords.
    pub octave: Option<i32>,
    /// Raw `prog:` chord tokens, in order. Resolved against the key on load.
    pub prog: Vec<heapless::String<24>, MAX_PROG>,
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
        key: heapless::String::new(),
        octave: None,
        prog: Vec::new(),
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
        if let Some(rest) = line.strip_prefix("prog:") {
            grid.prog = tokenize_prog::<MAX_PROG>(rest);
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
        || (!grid.stab.is_empty() && grid.stab.len() != grid.steps);
    if any_voice_mismatch {
        return Err(ParseError::VoiceLengthMismatch);
    }
    // Pad absent voices with silence so callers don't have to special-case them.
    pad_silent(&mut grid.kick, grid.steps);
    pad_silent(&mut grid.chat, grid.steps);
    pad_silent(&mut grid.ohat, grid.steps);
    pad_silent(&mut grid.stab, grid.steps);

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
        assert!((7..=9).contains(&hats), "expected ~8 hats in 1 s, got {hats}");
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
}
