//! Tape loss filter — physical HF-rolloff model.
//!
//! Ported from CHOWTape's `LossFilter.cpp`. Frequency-domain magnitude is
//! the product of three physical loss terms:
//!
//! ```text
//!   H(f) = exp(-k · spacing)                       (spacing loss)
//!        × (1 - exp(-k · thickness)) / (k · thickness)   (thickness loss)
//!        × sinc(k · gap / 2)                        (gap loss)
//! ```
//!
//! where `k = 2π·f / (speed · 0.0254)` is the spatial wave number on the
//! tape (speed in inches/sec converted to m/s). All physical dimensions
//! are in micrometres; `f` is the audio frequency in Hz.
//!
//! Magnitudes are sampled at `order` frequency bins, IDFT'd by summation
//! into a symmetric (linear-phase) FIR, then convolved sample-by-sample.
//! The FIR doubles its delay buffer so the inner loop has no modulo —
//! at ~140 taps × 2 channels this is well under 5 % CPU on STM32H7.
//!
//! Latency: `order / 2` samples.
//!
//! **No crossfade on param change yet** — twisting a knob will click. The
//! CHOWTape original crossfades between two FIRs over 1024 samples; we'll
//! port that if needed.

use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::TAU;
use libm::{cosf, expf, sinf};

/// Base FIR order at 44.1 kHz. Scales linearly with sample rate at
/// construction (`order = BASE_ORDER · fs / 44100`).
const BASE_ORDER: usize = 128;
/// Floor on the per-bin frequency in the wave-number calc, so the DC bin
/// doesn't divide by zero in `sinc(k·gap/2)` / `(1-exp)/x`.
const MIN_FREQ_HZ: f32 = 20.0;
const UM_TO_M: f32 = 1.0e-6;
const IPS_TO_MPS: f32 = 0.0254;

pub struct LossFilter {
    fs: f32,
    order: usize,
    coefs: Vec<f32>,
    /// Doubled-length ring buffer: write to `delay[w]` and `delay[w + order]`
    /// so the convolution can read `delay[w + order - k]` without modulo.
    delay: Vec<f32>,
    write_idx: usize,

    // Physical params (CHOWTape defaults: 30 IPS, 0.1 μm spacing/thick, 1 μm gap).
    speed_ips: f32,
    spacing_um: f32,
    thickness_um: f32,
    gap_um: f32,
    dirty: bool,
}

impl LossFilter {
    pub fn new(sample_rate: f32) -> Self {
        let order = (((BASE_ORDER as f32) * sample_rate / 44100.0) as usize).max(8);
        let mut f = Self {
            fs: sample_rate,
            order,
            coefs: vec![0.0; order],
            delay: vec![0.0; order * 2],
            write_idx: 0,
            speed_ips: 30.0,
            spacing_um: 0.1,
            thickness_um: 0.1,
            gap_um: 1.0,
            dirty: true,
        };
        f.recompute_coefs();
        f
    }

    pub fn order(&self) -> usize {
        self.order
    }

    /// Tape speed in inches/sec. Range 1-50, default 30.
    pub fn set_speed_ips(&mut self, ips: f32) {
        self.speed_ips = ips.clamp(1.0, 50.0);
        self.dirty = true;
    }
    /// Head-to-tape spacing in micrometres. Range 0.1-20, default 0.1.
    pub fn set_spacing_um(&mut self, um: f32) {
        self.spacing_um = um.clamp(0.1, 20.0);
        self.dirty = true;
    }
    /// Tape coating thickness in micrometres. Range 0.1-50, default 0.1.
    pub fn set_thickness_um(&mut self, um: f32) {
        self.thickness_um = um.clamp(0.1, 50.0);
        self.dirty = true;
    }
    /// Playhead gap in micrometres. Range 1-50, default 1.
    pub fn set_gap_um(&mut self, um: f32) {
        self.gap_um = um.clamp(1.0, 50.0);
        self.dirty = true;
    }

    pub fn speed_ips(&self) -> f32 {
        self.speed_ips
    }

    /// Rebuild the FIR coefficient table from current physical parameters.
    fn recompute_coefs(&mut self) {
        let order = self.order;
        let bin_width = self.fs / order as f32;

        // Frequency-domain magnitude, length `order`. Real and symmetric:
        // h_mag[order-k-1] = h_mag[k], so we only fill the lower half.
        let mut h_mag = vec![0.0f32; order];
        for k in 0..(order / 2) {
            let freq = ((k as f32) * bin_width).max(MIN_FREQ_HZ);
            let wave_number = TAU * freq / (self.speed_ips * IPS_TO_MPS);

            let spacing_loss = expf(-wave_number * self.spacing_um * UM_TO_M);

            let thick_k = wave_number * self.thickness_um * UM_TO_M;
            let thickness_loss = if thick_k > 1.0e-6 {
                (1.0 - expf(-thick_k)) / thick_k
            } else {
                1.0
            };

            let gap_over_two = wave_number * self.gap_um * UM_TO_M / 2.0;
            let gap_loss = if gap_over_two > 1.0e-6 {
                sinf(gap_over_two) / gap_over_two
            } else {
                1.0
            };

            let mag = spacing_loss * thickness_loss * gap_loss;
            h_mag[k] = mag;
            h_mag[order - k - 1] = mag;
        }

        // IDFT-by-summation into a symmetric (linear-phase) FIR.
        // We fill indices [order/2, order) and mirror to [1, order/2].
        // Index 0 stays zero — matches CHOWTape's behaviour.
        let inv_order = 1.0 / order as f32;
        for n in 0..(order / 2) {
            let mut acc = 0.0f32;
            for k in 0..order {
                acc += h_mag[k] * cosf(TAU * (k * n) as f32 * inv_order);
            }
            let c = acc * inv_order;
            self.coefs[order / 2 + n] = c;
            self.coefs[order / 2 - n] = c;
        }

        self.dirty = false;
    }

    /// Convolve one mono buffer in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.dirty {
            self.recompute_coefs();
        }

        let order = self.order;
        for sample in buf.iter_mut() {
            // Write to both halves of the doubled ring so the read window
            // [write_idx+1 .. write_idx+order+1] is always contiguous.
            self.delay[self.write_idx] = *sample;
            self.delay[self.write_idx + order] = *sample;

            // y[n] = sum_k coefs[k] * x[n-k]
            //      = sum_k coefs[k] * delay[write_idx + order - k]
            let mut acc = 0.0f32;
            let base = self.write_idx + order;
            for k in 0..order {
                acc += self.coefs[k] * self.delay[base - k];
            }
            *sample = acc;

            self.write_idx += 1;
            if self.write_idx >= order {
                self.write_idx = 0;
            }
        }
    }
}
