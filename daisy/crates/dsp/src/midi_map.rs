//! Flexible MIDI CC → parameter routing.
//!
//! Bindings live in a fixed-size 128-entry array indexed by CC number — no
//! allocator, O(1) lookup, identical behaviour on host and firmware. Add a
//! `Param` variant when adding a new mappable knob; `Engine::apply_param`
//! handles the dispatch.

/// Engine parameters that can be MIDI-mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    KickFreq,
    KickAccent,
    KickDecay,
    KickTone,
    KickAttackFm,
    KickSelfFm,
    KickDistDrive,
    ReverbWet,
    /// FM stab bus gain.
    StabGain,
    /// FM stab modulation index (brightness / harmonic richness).
    StabIndex,
    /// FM stab amp-envelope decay time, seconds (stab length).
    StabDecay,
    /// FM stab modulator:carrier frequency ratio.
    StabModRatio,
    /// FM stab operator self-feedback — the main grit/abrasion control.
    StabFeedback,
    /// FM stab pre-shaper drive (slams the waveshaper for more distortion).
    StabDrive,
    /// Tape "failure" amount (0 = pristine TC-250, 1 = eaten/falling apart).
    /// Drives 9 sub-stage params in concert through `TapeProcessor::set_failure`.
    TapeFailure,
    /// Stab ping-pong delay: wet amount folded into the master (0..1).
    StabDelayWet,
    /// Stab ping-pong delay: feedback / number of repeats (0..0.95).
    StabDelayFeedback,
    /// Stab ping-pong delay: delay time, seconds.
    StabDelayTime,
}

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub param: Param,
    pub min: f32,
    pub max: f32,
}

pub struct MidiMap {
    cc_bindings: [Option<Binding>; 128],
}

impl MidiMap {
    pub const fn new() -> Self {
        Self {
            cc_bindings: [None; 128],
        }
    }

    /// Bind a MIDI CC# to an engine parameter. The raw 0-127 CC value is
    /// linearly mapped onto `[min, max]`.
    pub fn bind_cc(&mut self, cc: u8, param: Param, min: f32, max: f32) {
        self.cc_bindings[cc as usize] = Some(Binding { param, min, max });
    }

    pub fn unbind_cc(&mut self, cc: u8) {
        self.cc_bindings[cc as usize] = None;
    }

    /// Resolve a CC#/value to a (param, mapped_value), or `None` if the CC
    /// is unbound.
    pub fn map_cc(&self, cc: u8, value: u8) -> Option<(Param, f32)> {
        let b = self.cc_bindings[cc as usize].as_ref()?;
        let n = value as f32 / 127.0;
        Some((b.param, b.min + n * (b.max - b.min)))
    }

    /// Iterate over all currently-bound CCs (useful for logging at startup).
    pub fn iter_bindings(&self) -> impl Iterator<Item = (u8, &Binding)> {
        self.cc_bindings
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.as_ref().map(|b| (i as u8, b)))
    }
}

impl Default for MidiMap {
    fn default() -> Self {
        Self::new()
    }
}
