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
    /// Rumble-bass lowpass cutoff, Hz.
    BassCutoff,
    /// Rumble-bass filter resonance (0..1).
    BassRes,
    /// Rumble-bass envelope→cutoff sweep amount, Hz.
    BassEnvMod,
    /// Rumble-bass output gain.
    BassGain,
    /// Master freeze wet/dry (0 = passthrough, 1 = held grain). Mirrors the
    /// visualizer's frame-freeze; the transport that drives it is unconnected.
    Freeze,
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

/// Install the standard kiosk CC -> Param bindings. Called from both the Mac
/// host and the Daisy firmware so a given CC means the same thing in both
/// environments — keep this the single source of truth (don't inline bindings).
///
/// CC 23 = TapeFailure and CC 24 = Freeze are the two the kiosk drives from the
/// Pi (distance and the browser freeze); the rest are hardware-knob bindings
/// the host uses and the firmware ignores (no synth Engine there yet).
pub fn install_kiosk_bindings(map: &mut MidiMap) {
    map.bind_cc(12, Param::KickAccent, 0.0, 1.0);
    map.bind_cc(13, Param::KickDecay, 0.0, 1.0);
    map.bind_cc(15, Param::KickAttackFm, 0.0, 1.0);
    map.bind_cc(16, Param::KickFreq, 30.0, 150.0);
    map.bind_cc(18, Param::KickTone, 0.0, 1.0);
    map.bind_cc(19, Param::KickSelfFm, 0.0, 1.0);
    map.bind_cc(21, Param::ReverbWet, 0.0, 1.0);
    map.bind_cc(22, Param::KickDistDrive, 1.0, 6.0);
    map.bind_cc(23, Param::TapeFailure, 0.0, 1.0); // 0 = pristine, 1 = eaten
    map.bind_cc(24, Param::Freeze, 0.0, 1.0); // 0 = passthrough, 1 = held grain
}
