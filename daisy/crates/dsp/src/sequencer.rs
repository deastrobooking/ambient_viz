//! 16-step (4 bars × 4/4) drum sequencer.
//!
//! Sample-accurate beat scheduling locked to a tempo curve from
//! [`crate::timeline`]. The sequencer maintains its own loop-relative
//! time and advances a `beat_phase` per sample, firing when the phase
//! crosses 1.0. The instantaneous BPM is sampled from the keypoint
//! curve each sample, so mid-loop tempo changes adjust the inter-beat
//! interval immediately without drift.
//!
//! Each step = one quarter note. 4 bars × 4 beats = 16 steps per loop.
//! Steps carry per-step **velocity** (0.0 = off, 1.0 = full). Default
//! pattern emphasises beats 1 & 3 (downbeats) and softens beats 2 & 4
//! ("backbeat" weak beats) at 0.9 — typical 4/4 accentuation.
//!
//! Beyond the on-beat kick triggers, the sequencer also fires hi-hat
//! events on the **upbeats** (half-way through each step's phase).
//! The very last upbeat of the loop (mid-way through step 15) emits an
//! open-hat event; every other upbeat is closed-hat.

use heapless::Vec;

use crate::timeline::{Keypoint, MAX_KEYPOINTS, bpm_at};

pub const STEPS_PER_LOOP: usize = 16;

/// One sample's worth of drum-trigger output from [`Sequencer::advance`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StepEvent {
    /// `Some(v)` = trigger the kick at velocity `v`. `None` = no kick.
    pub kick_velocity: Option<f32>,
    pub closed_hat: bool,
    pub open_hat: bool,
}

pub struct Sequencer {
    sample_rate: f32,
    /// Loop-relative playback time in seconds. Wraps at `loop_seconds`.
    time_seconds: f32,
    /// Length of one loop iteration. Set via [`Sequencer::set_tempo`].
    loop_seconds: f32,
    /// Sorted BPM keypoints from the timeline JSON.
    bpm_keypoints: Vec<Keypoint, MAX_KEYPOINTS>,
    /// Fractional position within the current beat, [0, 1).
    /// Initialised to 1.0 so the very first sample fires beat 0
    /// (downbeat-aligned on start and on every loop wrap).
    beat_phase: f32,
    /// Which step of the pattern fires next.
    step: u32,
    /// Per-step velocity. 0.0 = silent, >0.0 = velocity.
    pattern: [f32; STEPS_PER_LOOP],
    /// Has the current step's upbeat (phase ≥ 0.5) already fired? Reset
    /// when the step advances or the loop wraps.
    fired_upbeat: bool,
    /// If false, [`Sequencer::advance`] always returns a default (empty) event.
    enabled: bool,

    // Lifetime trigger counters (debug — host can poll & diff to see firing rate).
    kick_count: u64,
    closed_hat_count: u64,
    open_hat_count: u64,
}

/// Default 4/4 pattern: full velocity on beats 1 & 3, 0.7 on weak beats 2 & 4.
pub const DEFAULT_PATTERN: [f32; STEPS_PER_LOOP] = [
    1.0, 0.7, 1.0, 0.7, // bar 1
    1.0, 0.7, 1.0, 0.7, // bar 2
    1.0, 0.7, 1.0, 0.7, // bar 3
    1.0, 0.7, 1.0, 0.7, // bar 4
];

impl Sequencer {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            time_seconds: 0.0,
            loop_seconds: 0.0,
            bpm_keypoints: Vec::new(),
            beat_phase: 1.0,
            step: 0,
            pattern: DEFAULT_PATTERN,
            fired_upbeat: false,
            enabled: false,
            kick_count: 0,
            closed_hat_count: 0,
            open_hat_count: 0,
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

    /// Configure the tempo curve and loop length. Enables the sequencer.
    pub fn set_tempo(&mut self, keypoints: Vec<Keypoint, MAX_KEYPOINTS>, loop_seconds: f32) {
        self.bpm_keypoints = keypoints;
        self.loop_seconds = loop_seconds;
        self.enabled = true;
    }

    /// Replace the velocity pattern. Length up to [`STEPS_PER_LOOP`]; extras
    /// are ignored, missing steps default to silent.
    pub fn set_pattern(&mut self, pattern: &[f32]) {
        self.pattern = [0.0; STEPS_PER_LOOP];
        for (dst, src) in self.pattern.iter_mut().zip(pattern.iter()) {
            *dst = src.clamp(0.0, 1.0);
        }
    }

    pub fn set_step_velocity(&mut self, idx: usize, velocity: f32) {
        if idx < STEPS_PER_LOOP {
            self.pattern[idx] = velocity.clamp(0.0, 1.0);
        }
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
        self.beat_phase = 1.0;
        self.fired_upbeat = false;
    }

    /// Advance one audio sample. Returns a [`StepEvent`] describing any
    /// drum triggers that fire on this sample. Call exactly once per
    /// output sample.
    pub fn advance(&mut self) -> StepEvent {
        let mut evt = StepEvent::default();
        if !self.enabled {
            return evt;
        }

        self.time_seconds += 1.0 / self.sample_rate;
        if self.loop_seconds > 0.0 && self.time_seconds >= self.loop_seconds {
            self.time_seconds -= self.loop_seconds;
            self.step = 0;
            self.beat_phase = 1.0;
            self.fired_upbeat = false;
        }

        let bpm = bpm_at(&self.bpm_keypoints, self.time_seconds);
        let beats_per_sample = bpm / 60.0 / self.sample_rate;
        self.beat_phase += beats_per_sample;

        if self.beat_phase >= 1.0 {
            // Beat boundary — fire kick (if pattern velocity > 0) and step.
            self.beat_phase -= 1.0;
            let vel = self.pattern[self.step as usize];
            if vel > 0.0 {
                evt.kick_velocity = Some(vel);
                self.kick_count += 1;
            }
            self.step = (self.step + 1) % STEPS_PER_LOOP as u32;
            self.fired_upbeat = false;
            // Closed hats on every beat too
            evt.closed_hat = true;
        } else if !self.fired_upbeat && self.beat_phase >= 0.5 {
            // Upbeat (half-way through this step) — fire hi-hat.
            // Alternate open and closed hi hats
            self.fired_upbeat = true;
            if self.step % 2 == 0 {
                evt.open_hat = true;
                self.open_hat_count += 1;
            } else {
                evt.closed_hat = true;
                self.closed_hat_count += 1;
            }
        }

        evt
    }
}
