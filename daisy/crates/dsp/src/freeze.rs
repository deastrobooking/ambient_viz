//! Master "freeze" — captures a short grain of the program and loops it, the
//! audio analogue of the visualizer's frame-freeze. Built as a **parallel
//! send**: the live master keeps playing untouched while the held grain is run
//! through a stripped "failure tape" ([`GlitchTape`]) and summed *under* it, so
//! you get the composition continuing with a degraded, wobbling ghost of a
//! caught moment hovering on top.
//!
//! [`Freeze`] is the grain producer. Driven by `amount` in `[0, 1]`:
//! - `0.0` — idle. The ring keeps rolling (always holds the last
//!   [`LOOP_SECONDS`] of master) so a freeze can latch instantly; the send is
//!   silent.
//! - `> 0.0` — active. The ring is frozen and looped into the send; `amount`
//!   sets the ghost's level. A ~[`FADE_SECONDS`] slew on the level keeps the
//!   layer from popping in/out.
//!
//! Click-free looping uses **two read heads half a loop apart**, each
//! smootherstep-windowed (a raised-cosine-equivalent crossfade); the windows
//! sum to exactly `1.0` and have zero derivative at the seam and peak. The read
//! index is offset by the frozen `write` position so each window's zero-gain
//! point lands on the **ring seam** (the wrap between newest and oldest captured
//! sample) — that masks the seam and blends the grain's end into its start. The
//! looped grain is then run through a gentle one-pole softener that rounds the
//! repeated transients (the "clippy" bite) before the level fade.
//!
//! The producer is cheap (records the ring, or reads the loop); the expensive
//! glitch-tape + sum are gated by [`Freeze::active`] so they run only while a
//! freeze is up. Stereo-interleaved; no allocation after construction. On the
//! embedded target the ring buffer belongs in SDRAM like the other FX buffers.

use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::TAU;
use libm::expf;

use crate::tape::{Chew, WowFlutter};

/// Loop-grain length, seconds. Long enough to read as a held texture rather
/// than a pitched buzz; the overlap-add smears the loop period further.
pub const LOOP_SECONDS: f32 = 0.3;

/// Level fade time, seconds — ramps the ghost in/out so it doesn't pop.
const FADE_SECONDS: f32 = 0.01;

/// Grain softener cutoff, Hz. A one-pole low-pass on the looped grain that
/// rounds the sharp attacks which would otherwise repeat at the loop rate and
/// read as a "chopped transient" / clippy bite. Lower = softer, duller ghost.
const GRAIN_SOFTEN_HZ: f32 = 3000.0;

/// Below this level the freeze is considered inactive (record, skip glitch/sum).
const EPS: f32 = 1.0e-4;

/// Trim on the ghost return so the held/glitched grain sits *under* the live
/// master ("a bit of glitch on top"), not level with it.
pub const FREEZE_RETURN_GAIN: f32 = 0.6;

pub struct Freeze {
    /// Stereo-interleaved ring of the last [`LOOP_SECONDS`] of audio.
    buf: Vec<f32>,
    /// Loop length, frames.
    frames: usize,
    /// Half-loop read-head offset, frames.
    half: usize,
    /// Write head (frames) — advances only while idle, so the grain is the
    /// audio captured at the moment of the freeze.
    write: usize,
    /// Read phase (frames) — advances only while active.
    read: f32,
    /// Target ghost level (the commanded `amount`).
    amount: f32,
    /// Smoothed ghost level — slews toward `amount` over [`FADE_SECONDS`].
    level: f32,
    /// Per-sample one-pole fade coefficient.
    fade: f32,
    /// Per-channel one-pole LPF state for the grain softener.
    lpf_l: f32,
    lpf_r: f32,
    /// Per-sample one-pole coefficient for the grain softener.
    soften: f32,
}

impl Freeze {
    pub fn new(sample_rate: f32) -> Self {
        let frames = (LOOP_SECONDS * sample_rate).max(2.0) as usize;
        Self {
            buf: vec![0.0; frames * 2],
            frames,
            half: frames / 2,
            write: 0,
            read: 0.0,
            amount: 0.0,
            level: 0.0,
            fade: 1.0 - expf(-1.0 / (FADE_SECONDS * sample_rate)),
            lpf_l: 0.0,
            lpf_r: 0.0,
            soften: 1.0 - expf(-TAU * GRAIN_SOFTEN_HZ / sample_rate),
        }
    }

    /// Set the freeze amount in `[0, 1]` (the ghost return level). A rising edge
    /// from 0 latches the current grain (the ring already holds the last
    /// [`LOOP_SECONDS`]) and starts the loop from phase 0.
    pub fn set_amount(&mut self, amount: f32) {
        let a = amount.clamp(0.0, 1.0);
        if a > 0.0 && self.amount == 0.0 {
            self.read = 0.0; // rising edge: start the loop from the seam
            self.lpf_l = 0.0; // clear softener memory so the grain eases in clean
            self.lpf_r = 0.0;
        }
        self.amount = a;
    }

    pub fn amount(&self) -> f32 {
        self.amount
    }

    /// True while the ghost is sounding (commanded on, or still fading out).
    /// The engine gates the glitch-tape + sum on this so they cost nothing at
    /// rest.
    pub fn active(&self) -> bool {
        self.amount > 0.0 || self.level > EPS
    }

    /// Roll the grain ring with the live master and emit the ghost send.
    /// `master` is read-only (recorded while idle); `send` (same length) is
    /// filled with the looped grain × level while active, or silence when idle.
    pub fn process(&mut self, master: &[f32], send: &mut [f32]) {
        let n = self.frames as f32;
        for (m, s) in master.chunks_exact(2).zip(send.chunks_exact_mut(2)) {
            self.level += self.fade * (self.amount - self.level);

            if !(self.amount > 0.0 || self.level > EPS) {
                // Idle: roll the ring, silent send.
                self.buf[2 * self.write] = m[0];
                self.buf[2 * self.write + 1] = m[1];
                self.write += 1;
                if self.write >= self.frames {
                    self.write = 0;
                }
                s[0] = 0.0;
                s[1] = 0.0;
                continue;
            }

            // Two heads, half a loop apart, triangular windows summing to 1.0.
            // Offset the index by `write` (the frozen ring seam) so the window
            // zeros land on the seam and blend the grain's end into its start.
            let base = self.write;
            let pa = (base + self.read as usize) % self.frames;
            let mut rb = self.read + self.half as f32;
            if rb >= n {
                rb -= n;
            }
            let pb = (base + rb as usize) % self.frames;

            // Triangular crossfade weight for head A; head B is its complement.
            let va = 2.0 * (self.read / n) - 1.0;
            let tri_a = 1.0 - if va < 0.0 { -va } else { va };
            // Raised-cosine-equivalent smoothing: smootherstep the triangle.
            // S(t)+S(1-t)=1, so the pair still sums to exactly 1.0, but the
            // derivative is flattened to zero at both the loop seam and the
            // window peak (C2) — softer than the bare triangular corner, with
            // no per-sample cosf (cf. the tape's fast-cos cost note).
            let ga = tri_a * tri_a * tri_a * (tri_a * (tri_a * 6.0 - 15.0) + 10.0);
            let gb = 1.0 - ga;

            // Pre-soften the grain: a one-pole LPF rounds the sharp attacks that
            // otherwise repeat twice per loop (~6.7 Hz here) — the source of the
            // "chopped transient" bite.
            let dry_l = self.buf[2 * pa] * ga + self.buf[2 * pb] * gb;
            let dry_r = self.buf[2 * pa + 1] * ga + self.buf[2 * pb + 1] * gb;
            self.lpf_l += self.soften * (dry_l - self.lpf_l);
            self.lpf_r += self.soften * (dry_r - self.lpf_r);

            s[0] = self.lpf_l * self.level;
            s[1] = self.lpf_r * self.level;

            self.read += 1.0;
            if self.read >= n {
                self.read -= n;
            }
        }
    }
}

/// Stripped "failure tape" for the freeze ghost — only the glitchy stages
/// (wow/flutter wobble + chew dropouts) at eaten-tape settings. No loss FIR,
/// compressor, head bump or hysteresis: cheaper than the master tape, and the
/// engine runs it only while a freeze is active.
pub struct GlitchTape {
    wow_flutter: WowFlutter,
    chew: Chew,
}

// Eaten-tape character for the ghost (cf. tape's FAILURE_DESTROYED).
const GLITCH_WOW_RATE_HZ: f32 = 3.0;
const GLITCH_WOW_DEPTH_MS: f32 = 12.0;
const GLITCH_FLUTTER_DEPTH_MS: f32 = 1.2;
const GLITCH_CHEW_DEPTH: f32 = 0.8;
const GLITCH_CHEW_FREQ: f32 = 0.7;
const GLITCH_CHEW_VARIANCE: f32 = 0.5;

impl GlitchTape {
    pub fn new(sample_rate: f32) -> Self {
        let mut wow_flutter = WowFlutter::new(sample_rate);
        wow_flutter.set_wow_rate_hz(GLITCH_WOW_RATE_HZ);
        wow_flutter.set_wow_depth_ms(GLITCH_WOW_DEPTH_MS);
        wow_flutter.set_flutter_depth_ms(GLITCH_FLUTTER_DEPTH_MS);

        let mut chew = Chew::new(sample_rate);
        chew.set_depth(GLITCH_CHEW_DEPTH);
        chew.set_freq(GLITCH_CHEW_FREQ);
        chew.set_variance(GLITCH_CHEW_VARIANCE);

        Self { wow_flutter, chew }
    }

    /// Process the ghost send in place (stereo interleaved).
    pub fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            let (mut l, mut r) = self.wow_flutter.process_sample(frame[0], frame[1]);
            self.chew.process_sample(&mut l, &mut r);
            frame[0] = l;
            frame[1] = r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn frames() -> usize {
        (LOOP_SECONDS * SR) as usize
    }

    #[test]
    fn idle_send_is_silent_and_master_untouched() {
        let mut f = Freeze::new(SR);
        let master = vec![0.3, -0.4, 0.5, -0.6]; // two stereo frames
        let orig = master.clone();
        let mut send = vec![9.0; 4];
        f.process(&master, &mut send);
        assert_eq!(master, orig, "freeze must not modify the dry master");
        assert!(send.iter().all(|&x| x == 0.0), "idle send must be silent");
        assert!(!f.active());
    }

    #[test]
    fn frozen_grain_holds_at_constant_amplitude() {
        let mut f = Freeze::new(SR);
        let frames = frames();
        // Fill the ring with a constant 1.0 while idle.
        let fill = vec![1.0_f32; (frames + 8) * 2];
        let mut sink = vec![0.0_f32; (frames + 8) * 2];
        f.process(&fill, &mut sink);

        // Freeze and read the looped grain (master is irrelevant when frozen).
        f.set_amount(1.0);
        assert!(f.active());
        let master = vec![0.0_f32; frames * 4];
        let mut send = vec![0.0_f32; frames * 4];
        f.process(&master, &mut send);
        // After the level fade settles, the constant grain holds ~1.0 — proving
        // the two triangular heads sum to 1.0 (constant amplitude, click-free).
        for &s in send.iter().skip(2 * 4000) {
            assert!((s - 1.0).abs() < 1e-3, "frozen grain should hold ~1.0, got {s}");
        }

        // Release → ring rolls again, send goes silent.
        f.set_amount(0.0);
        let master = vec![0.2_f32; frames * 2];
        let mut send = vec![0.0_f32; frames * 2];
        f.process(&master, &mut send);
        assert!(!f.active(), "freeze should be inactive after release settles");
        assert!(
            send.iter().rev().take(8).all(|&x| x == 0.0),
            "released send must be silent"
        );
    }

    #[test]
    fn loop_seam_is_blended_no_jump() {
        // Capture a monotonic ramp longer than the loop so `write` ends mid-
        // buffer (seam ≠ index 0) and the grain is smooth *except* at the seam,
        // where newest→oldest would jump ~0.75 if the windows didn't mask it.
        let mut f = Freeze::new(SR);
        let frames = frames();
        let total = frames + frames / 3; // leaves write at frames/3, not 0
        let mut fill = vec![0.0_f32; total * 2];
        for i in 0..total {
            let v = i as f32 / total as f32;
            fill[2 * i] = v;
            fill[2 * i + 1] = v;
        }
        let mut sink = vec![0.0_f32; total * 2];
        f.process(&fill, &mut sink);

        // Freeze, settle the level, then scan the send for the largest sample-
        // to-sample jump. Masked seam → only the ramp's tiny slope + the smooth
        // crossfade; an unaligned seam would spike a ~0.75 jump per loop.
        f.set_amount(1.0);
        let master = vec![0.0_f32; frames * 4];
        let mut send = vec![0.0_f32; frames * 4];
        f.process(&master, &mut send);
        let mut max_jump = 0.0_f32;
        // skip the fade-in region (left channel only)
        let start = 2000;
        for i in (start + 1)..(send.len() / 2) {
            let d = (send[2 * i] - send[2 * i - 2]).abs();
            if d > max_jump {
                max_jump = d;
            }
        }
        assert!(
            max_jump < 0.1,
            "loop seam should be blended, max consecutive jump was {max_jump}"
        );
    }
}
