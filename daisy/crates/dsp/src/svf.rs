//! State Variable Filter — double-sampled, stable.
//!
//! Ported from DaisySP `Source/Filters/svf.cpp`.
//! Original credit: Andrew Simper (musicdsp.org), stability limit from
//! Laurent de Soras, notch output fix from Stefan Diedrichsen,
//! C++ port by Stephen Hensley.
//!
//! Unlike infinitedsp's TPT/ZDF SVF, this one is double-sampled (runs the
//! integrator pair twice per input sample and averages) which gives it
//! different stability and timbre characteristics. The analog bass drum
//! is tuned to *this* response, which is why we don't substitute
//! infinitedsp's SVF here.

use core::f32::consts::PI;
use libm::{powf, sinf};

pub struct Svf {
    sr: f32,
    fc: f32,
    res: f32,
    drive: f32,
    pre_drive: f32,
    freq: f32,
    damp: f32,

    notch: f32,
    low: f32,
    high: f32,
    band: f32,

    out_low: f32,
    out_high: f32,
    out_band: f32,
    out_notch: f32,
    out_peak: f32,

    fc_max: f32,
}

impl Svf {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            fc: 200.0,
            res: 0.5,
            drive: 0.5,
            pre_drive: 0.5,
            freq: 0.25,
            damp: 0.0,
            notch: 0.0,
            low: 0.0,
            high: 0.0,
            band: 0.0,
            out_low: 0.0,
            out_high: 0.0,
            out_band: 0.0,
            out_notch: 0.0,
            out_peak: 0.0,
            fc_max: sample_rate / 3.0,
        }
    }

    /// Set cutoff frequency in Hz. Must be in [0, sample_rate/3].
    pub fn set_freq(&mut self, f: f32) {
        self.fc = f.clamp(1.0e-6, self.fc_max);
        // double-sampled, so use fs*2 in the denominator
        self.freq = 2.0 * sinf(PI * f32::min(0.25, self.fc / (self.sr * 2.0)));
        self.recompute_damp();
    }

    /// Set resonance. Must be in [0, 1] for stability.
    pub fn set_res(&mut self, r: f32) {
        self.res = r.clamp(0.0, 1.0);
        self.recompute_damp();
        self.drive = self.pre_drive * self.res;
    }

    /// Set drive (affects resonance response).
    pub fn set_drive(&mut self, d: f32) {
        self.pre_drive = (d * 0.1).clamp(0.0, 1.0);
        self.drive = self.pre_drive * self.res;
    }

    pub fn process(&mut self, input: f32) {
        // First pass
        self.notch = input - self.damp * self.band;
        self.low += self.freq * self.band;
        self.high = self.notch - self.low;
        self.band = self.freq * self.high + self.band
            - self.drive * self.band * self.band * self.band;

        self.out_low = 0.5 * self.low;
        self.out_high = 0.5 * self.high;
        self.out_band = 0.5 * self.band;
        self.out_peak = 0.5 * (self.low - self.high);
        self.out_notch = 0.5 * self.notch;

        // Second pass
        self.notch = input - self.damp * self.band;
        self.low += self.freq * self.band;
        self.high = self.notch - self.low;
        self.band = self.freq * self.high + self.band
            - self.drive * self.band * self.band * self.band;

        self.out_low += 0.5 * self.low;
        self.out_high += 0.5 * self.high;
        self.out_band += 0.5 * self.band;
        self.out_peak += 0.5 * (self.low - self.high);
        self.out_notch += 0.5 * self.notch;
    }

    pub fn low(&self) -> f32 { self.out_low }
    pub fn high(&self) -> f32 { self.out_high }
    pub fn band(&self) -> f32 { self.out_band }
    pub fn notch(&self) -> f32 { self.out_notch }
    pub fn peak(&self) -> f32 { self.out_peak }

    fn recompute_damp(&mut self) {
        self.damp = f32::min(
            2.0 * (1.0 - powf(self.res, 0.25)),
            f32::min(2.0, 2.0 / self.freq - self.freq * 0.5),
        );
    }
}
