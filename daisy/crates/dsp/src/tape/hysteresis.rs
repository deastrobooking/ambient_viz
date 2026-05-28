//! Jiles-Atherton magnetic-hysteresis model — the actual tape "saturation".
//!
//! Ported from CHOWTape's `HysteresisProcessing.h` + `HysteresisOps.h` (which
//! in turn implements the DAFx 2019 paper). This is the one piece of CHOWTape
//! that has no `infinitedsp` analogue — soft-clip / tanh can't capture
//! hysteresis because the latter has **memory**: the magnetization curve
//! depends on whether the input was just increasing or decreasing, giving
//! an asymmetric loop in time.
//!
//! Implementation choices:
//! - **RK2 solver** (sufficient quality; RK4 or Newton-Raphson are more
//!   accurate but ~2-4× the CPU; v1 stays simple).
//! - **No oversampling yet** — runs at audio rate. May alias on very bright
//!   content. The fix is upsample → process → low-pass → decimate, which
//!   we'll add in a follow-up if it's audible.
//! - **No SIMD** — the C++ original uses xsimd, we go scalar. On STM32H7
//!   the per-sample cost is ~2 `tanhf` calls; well under 1 % CPU at 48 kHz
//!   for stereo.
//! - **Float (f32) throughout**, vs CHOWTape's `double`. The model is
//!   well-conditioned at audio inputs and f32 is faster on embedded; if we
//!   see weird behaviour we can bump to f64.
//!
//! Cooked user-facing parameters (matching CHOWTape's `cook()`):
//! - `drive` 0..1 — gain into the saturator. 0 = barely saturating, 1 = hot.
//! - `width` 0..1 — hysteresis loop *area*. 0 = thin loop, 1 = wide loop.
//! - `sat` 0..1 — saturation magnetization `M_s` (inversely). 0 = max M_s
//!   (more headroom), 1 = min M_s (more clamping).

use libm::{sqrtf, tanhf};

/// Inter-domain coupling constant (Jiles-Atherton α). Tape's `alpha` in the
/// CHOWTape source is fixed at this value.
const ALPHA: f32 = 1.6e-3;
/// Alpha-transform coefficient for the input-derivative estimator.
const D_ALPHA: f32 = 0.75;
const ONE_THIRD: f32 = 1.0 / 3.0;
const NEAR_ZERO_THRESHOLD: f32 = 0.001;

pub struct Hysteresis {
    // ----- cooked JA parameters -----
    m_s: f32,
    a: f32,
    c: f32,
    k: f32,

    // ----- precomputed combinations -----
    nc: f32,
    m_s_oa_tc: f32,
    m_s_oa_tc_talpha: f32,

    // ----- per-sample state -----
    m_n1: f32,
    h_n1: f32,
    h_d_n1: f32,

    // ----- sample timing -----
    t: f32,
    upper_lim: f32,
}

impl Hysteresis {
    pub fn new(sample_rate: f32) -> Self {
        let mut h = Self {
            m_s: 0.0,
            a: 0.0,
            c: 0.0,
            k: 0.0,
            nc: 0.0,
            m_s_oa_tc: 0.0,
            m_s_oa_tc_talpha: 0.0,
            m_n1: 0.0,
            h_n1: 0.0,
            h_d_n1: 0.0,
            t: 1.0 / sample_rate,
            upper_lim: 20.0,
        };
        h.cook(0.5, 0.5, 0.5);
        h
    }

    pub fn set_sample_rate(&mut self, fs: f32) {
        self.t = 1.0 / fs;
    }

    pub fn reset(&mut self) {
        self.m_n1 = 0.0;
        self.h_n1 = 0.0;
        self.h_d_n1 = 0.0;
    }

    /// Set user-facing parameters. All in `[0, 1]`.
    pub fn cook(&mut self, drive: f32, width: f32, sat: f32) {
        let drive = drive.clamp(0.0, 1.0);
        let width = width.clamp(0.0, 1.0);
        let sat = sat.clamp(0.0, 1.0);

        self.m_s = 0.5 + 1.5 * (1.0 - sat);
        self.a = self.m_s / (0.01 + 6.0 * drive);
        // sqrt(1-width) - 0.01 can theoretically be negative when width≈1;
        // clamp to keep `nc = 1-c` and the f1_denom math well-behaved.
        self.c = (sqrtf((1.0 - width).max(0.0)) - 0.01).max(0.0);
        self.k = 0.47875;
        self.upper_lim = 20.0;

        self.nc = 1.0 - self.c;
        let m_s_oa = self.m_s / self.a;
        self.m_s_oa_tc = self.c * m_s_oa;
        self.m_s_oa_tc_talpha = ALPHA * self.m_s_oa_tc;
    }

    /// `dM/dt` at the given state. Pure function; doesn't mutate `self`.
    #[inline]
    fn hysteresis_func(&self, m: f32, h: f32, h_d: f32) -> f32 {
        let q = (h + m * ALPHA) / self.a;
        let near_zero = q.abs() < NEAR_ZERO_THRESHOLD;

        // Langevin function + derivative. The `near_zero` Taylor expansions
        // avoid the 1/q blow-up at the origin.
        let (langevin_val, l_prime) = if near_zero {
            (q * ONE_THIRD, ONE_THIRD)
        } else {
            let one_over_q = 1.0 / q;
            let coth = 1.0 / tanhf(q);
            let one_over_q_sq = one_over_q * one_over_q;
            let coth_sq = coth * coth;
            (coth - one_over_q, one_over_q_sq - coth_sq + 1.0)
        };

        let m_diff = langevin_val * self.m_s - m;

        // δ = sign(H_d); δ_M = (sign(δ) == sign(M_diff)).
        // Using a 3-valued sign (matches CHOWTape) so δ_M is false when
        // either input is exactly zero — avoids a spurious branch at the
        // tip of the hysteresis loop.
        let delta: f32 = if h_d >= 0.0 { 1.0 } else { -1.0 };
        let m_diff_sign = if m_diff > 0.0 {
            1.0
        } else if m_diff < 0.0 {
            -1.0
        } else {
            0.0
        };
        let delta_m = delta == m_diff_sign;
        let kap1 = if delta_m { self.nc } else { 0.0 };

        let f1_denom = (self.nc * delta) * self.k - ALPHA * m_diff;
        let f1 = kap1 * m_diff / f1_denom;
        let f2 = l_prime * self.m_s_oa_tc;
        let f3 = 1.0 - l_prime * self.m_s_oa_tc_talpha;

        h_d * (f1 + f2) / f3
    }

    /// Process one sample. Input `h` is the magnetic field (i.e. the audio
    /// sample); output `M` is the tape's magnetization (the saturated audio).
    pub fn process_sample(&mut self, h: f32) -> f32 {
        // Estimate H_d via alpha-transform (numerical derivative with built-in
        // smoothing — avoids the noise of plain backward difference).
        let h_d = ((1.0 + D_ALPHA) / self.t) * (h - self.h_n1) - D_ALPHA * self.h_d_n1;

        // --- RK2 ---
        let k1 = self.hysteresis_func(self.m_n1, self.h_n1, self.h_d_n1) * self.t;
        let k2 = self.hysteresis_func(
            self.m_n1 + k1 * 0.5,
            (h + self.h_n1) * 0.5,
            (h_d + self.h_d_n1) * 0.5,
        ) * self.t;
        let mut m = self.m_n1 + k2;

        // Stability guard — reset rather than emit a NaN/runaway sample.
        // CHOWTape does the same; usually only fires on pathological input.
        let mut h_d_save = h_d;
        if m.is_nan() || m.abs() > self.upper_lim {
            m = 0.0;
            h_d_save = 0.0;
        }

        self.m_n1 = m;
        self.h_n1 = h;
        self.h_d_n1 = h_d_save;
        m
    }
}
