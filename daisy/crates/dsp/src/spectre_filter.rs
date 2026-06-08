//! Standalone Spectre-inspired filter core.
//!
//! This is a small `no_std` port of the useful standalone pieces from the
//! sibling Spectre-Filter project. It deliberately excludes plugin buses,
//! analyzer/UI state, dynamic allocation, and DAW sidechain assumptions.

use core::f32::consts::PI;

/// Master filter models shared with the Spectre/Nexus vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterFilterModel {
    Off,
    CleanLowPass,
    Ladder12,
    Ladder24,
    Diode,
    SemMorph,
}

/// Runtime settings for the standalone filter insert.
#[derive(Debug, Clone, Copy)]
pub struct MasterFilterSettings {
    pub model: MasterFilterModel,
    pub cutoff_hz: f32,
    pub resonance: f32,
    pub drive: f32,
    pub morph: f32,
    pub mix: f32,
}

impl Default for MasterFilterSettings {
    fn default() -> Self {
        Self {
            model: MasterFilterModel::Off,
            cutoff_hz: 18_000.0,
            resonance: 0.12,
            drive: 0.0,
            morph: 0.0,
            mix: 0.0,
        }
    }
}

/// Simple attack/release envelope follower shared by dynamic bands and future
/// host/editor meters.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeFollower {
    sample_rate: f32,
    attack_coeff: f32,
    release_coeff: f32,
    value: f32,
}

impl EnvelopeFollower {
    pub fn new(sample_rate: f32) -> Self {
        let mut f = Self {
            sample_rate,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            value: 0.0,
        };
        f.configure(sample_rate, 10.0, 120.0);
        f
    }

    pub fn configure(&mut self, sample_rate: f32, attack_ms: f32, release_ms: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.attack_coeff = coefficient(self.sample_rate, attack_ms.max(0.05));
        self.release_coeff = coefficient(self.sample_rate, release_ms.max(1.0));
    }

    pub fn reset(&mut self) {
        self.value = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let rectified = input.abs().min(8.0);
        let coeff = if rectified > self.value {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.value = coeff * self.value + (1.0 - coeff) * rectified;
        self.value
    }
}

pub const DYNAMIC_BAND_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandMode {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    Stereo,
    Mid,
    Side,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
pub struct DynamicBandSettings {
    pub enabled: bool,
    pub mode: BandMode,
    pub channel_mode: ChannelMode,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
    /// Envelope-scaled extra gain in dB. Negative values duck; positive values
    /// expand. Applied as `gain_db + dynamic_db * envelope`.
    pub dynamic_db: f32,
    /// Envelope-scaled cutoff motion in octaves.
    pub sweep_octaves: f32,
    pub env_attack_ms: f32,
    pub env_release_ms: f32,
    pub env_sensitivity: f32,
}

impl Default for DynamicBandSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: BandMode::Bell,
            channel_mode: ChannelMode::Stereo,
            frequency_hz: 1_000.0,
            gain_db: 0.0,
            q: 0.707,
            dynamic_db: 0.0,
            sweep_octaves: 0.0,
            env_attack_ms: 10.0,
            env_release_ms: 120.0,
            env_sensitivity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DynamicFilterSettings {
    pub bands: [DynamicBandSettings; DYNAMIC_BAND_COUNT],
}

impl Default for DynamicFilterSettings {
    fn default() -> Self {
        Self {
            bands: [DynamicBandSettings::default(); DYNAMIC_BAND_COUNT],
        }
    }
}

/// Eight-band zero-latency dynamic filter core. Coefficients are recalculated
/// per sample for now so dynamic sweeps are smooth and bounded; later passes
/// can cache/smooth per band if profiling says this is too expensive.
#[derive(Debug, Clone, Copy)]
pub struct DynamicFilter {
    sample_rate: f32,
    bands: [DynamicBand; DYNAMIC_BAND_COUNT],
}

impl DynamicFilter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            bands: [DynamicBand::new(); DYNAMIC_BAND_COUNT],
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.reset();
    }

    pub fn reset(&mut self) {
        for band in &mut self.bands {
            band.reset();
        }
    }

    pub fn envelope_values(&self) -> [f32; DYNAMIC_BAND_COUNT] {
        core::array::from_fn(|idx| self.bands[idx].envelope_value())
    }

    pub fn process_buffer(&mut self, buffer: &mut [f32], settings: &DynamicFilterSettings) {
        for frame in buffer.chunks_exact_mut(2) {
            let (l, r) = self.process_stereo(frame[0], frame[1], settings);
            frame[0] = l;
            frame[1] = r;
        }
    }

    #[inline]
    pub fn process_stereo(
        &mut self,
        mut left: f32,
        mut right: f32,
        settings: &DynamicFilterSettings,
    ) -> (f32, f32) {
        for (band, band_settings) in self.bands.iter_mut().zip(settings.bands.iter()) {
            let out = band.process(left, right, self.sample_rate, *band_settings);
            left = out.0;
            right = out.1;
        }
        (sanitize_audio(left), sanitize_audio(right))
    }
}

#[derive(Debug, Clone, Copy)]
struct DynamicBand {
    left: Biquad,
    right: Biquad,
    envelope: EnvelopeFollower,
}

impl DynamicBand {
    const fn new() -> Self {
        Self {
            left: Biquad::new(),
            right: Biquad::new(),
            envelope: EnvelopeFollower {
                sample_rate: 48_000.0,
                attack_coeff: 0.0,
                release_coeff: 0.0,
                value: 0.0,
            },
        }
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.envelope.reset();
    }

    fn envelope_value(&self) -> f32 {
        self.envelope.value()
    }

    #[inline]
    fn process(
        &mut self,
        left: f32,
        right: f32,
        sample_rate: f32,
        settings: DynamicBandSettings,
    ) -> (f32, f32) {
        if !settings.enabled {
            return (sanitize_audio(left), sanitize_audio(right));
        }

        self.envelope
            .configure(sample_rate, settings.env_attack_ms, settings.env_release_ms);
        let detector = 0.5 * (left.abs() + right.abs());
        let env = (self.envelope.process(detector) * settings.env_sensitivity).clamp(0.0, 1.0);
        let freq = settings.frequency_hz
            * libm::powf(2.0, (settings.sweep_octaves * env).clamp(-4.0, 4.0));
        let gain_db = settings.gain_db + settings.dynamic_db * env;
        let coeffs = design_biquad(settings.mode, sample_rate, freq, gain_db, settings.q);

        match settings.channel_mode {
            ChannelMode::Stereo => {
                self.left.set_coeffs(coeffs);
                self.right.set_coeffs(coeffs);
                (self.left.process(left), self.right.process(right))
            }
            ChannelMode::Left => {
                self.left.set_coeffs(coeffs);
                (self.left.process(left), sanitize_audio(right))
            }
            ChannelMode::Right => {
                self.right.set_coeffs(coeffs);
                (sanitize_audio(left), self.right.process(right))
            }
            ChannelMode::Mid => {
                let mid = 0.5 * (left + right);
                let side = 0.5 * (left - right);
                self.left.set_coeffs(coeffs);
                let mid = self.left.process(mid);
                (sanitize_audio(mid + side), sanitize_audio(mid - side))
            }
            ChannelMode::Side => {
                let mid = 0.5 * (left + right);
                let side = 0.5 * (left - right);
                self.right.set_coeffs(coeffs);
                let side = self.right.process(side);
                (sanitize_audio(mid + side), sanitize_audio(mid - side))
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoeffs {
    const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Biquad {
    coeffs: BiquadCoeffs,
    z1: f32,
    z2: f32,
}

impl Biquad {
    const fn new() -> Self {
        Self {
            coeffs: BiquadCoeffs::identity(),
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn set_coeffs(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let input = sanitize_audio(input);
        let output = self.coeffs.b0 * input + self.z1;
        self.z1 = self.coeffs.b1 * input - self.coeffs.a1 * output + self.z2;
        self.z2 = self.coeffs.b2 * input - self.coeffs.a2 * output;
        sanitize_audio(output)
    }
}

/// Stereo master filter insert.
#[derive(Debug, Clone, Copy)]
pub struct MasterFilter {
    sample_rate: f32,
    left: MasterFilterChannel,
    right: MasterFilterChannel,
}

impl MasterFilter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            left: MasterFilterChannel::new(),
            right: MasterFilterChannel::new(),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }

    #[inline]
    pub fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        settings: MasterFilterSettings,
    ) -> (f32, f32) {
        if settings.model == MasterFilterModel::Off || settings.mix <= 0.000_01 {
            return (sanitize_audio(left), sanitize_audio(right));
        }

        (
            self.left.process(left, self.sample_rate, settings),
            self.right.process(right, self.sample_rate, settings),
        )
    }

    pub fn process_buffer(&mut self, buffer: &mut [f32], settings: MasterFilterSettings) {
        for frame in buffer.chunks_exact_mut(2) {
            let (l, r) = self.process_stereo(frame[0], frame[1], settings);
            frame[0] = l;
            frame[1] = r;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MasterFilterChannel {
    ladder_z: [f32; 4],
    sem_low: f32,
    sem_band: f32,
}

impl MasterFilterChannel {
    const fn new() -> Self {
        Self {
            ladder_z: [0.0; 4],
            sem_low: 0.0,
            sem_band: 0.0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    #[inline]
    fn process(&mut self, input: f32, sample_rate: f32, settings: MasterFilterSettings) -> f32 {
        let dry = sanitize_audio(input);
        let cutoff = settings.cutoff_hz.clamp(20.0, sample_rate * 0.45);
        let resonance = settings.resonance.clamp(0.0, 1.0);
        let drive = settings.drive.clamp(0.0, 1.0);
        let morph = settings.morph.clamp(0.0, 1.0);

        let wet = match settings.model {
            MasterFilterModel::Off => dry,
            MasterFilterModel::CleanLowPass => self.process_sem(
                dry,
                sample_rate,
                cutoff,
                resonance * 0.65,
                drive * 0.35,
                0.0,
            ),
            MasterFilterModel::Ladder12 => {
                self.process_ladder(dry, sample_rate, cutoff, resonance, drive, 2)
            }
            MasterFilterModel::Ladder24 => {
                self.process_ladder(dry, sample_rate, cutoff, resonance, drive, 4)
            }
            MasterFilterModel::Diode => {
                self.process_diode(dry, sample_rate, cutoff, resonance, drive)
            }
            MasterFilterModel::SemMorph => {
                self.process_sem(dry, sample_rate, cutoff, resonance, drive, morph)
            }
        };

        sanitize_audio(lerp(dry, wet, settings.mix.clamp(0.0, 1.0)))
    }

    #[inline]
    fn process_ladder(
        &mut self,
        input: f32,
        sample_rate: f32,
        cutoff: f32,
        resonance: f32,
        drive: f32,
        poles: usize,
    ) -> f32 {
        let g = libm::tanf(PI * cutoff / sample_rate);
        let g = g / (1.0 + g);
        let feedback = libm::powf(resonance, 1.12) * if poles == 2 { 1.85 } else { 3.95 };
        let saturation = (0.08 + resonance * 0.65 + drive * 0.75).clamp(0.08, 1.35);

        let mut stage = input * (1.0 + drive * 5.0) - feedback * self.ladder_z[poles - 1];
        stage = fast_tanh(stage);

        for z in self.ladder_z.iter_mut().take(poles) {
            let delta = fast_tanh((stage - *z) * saturation) * g;
            *z = sanitize_audio(*z + delta + delta);
            stage = fast_tanh(*z);
        }

        sanitize_audio(soft_clip(
            self.ladder_z[poles - 1],
            resonance * 0.18 + drive * 0.18,
        ))
    }

    #[inline]
    fn process_diode(
        &mut self,
        input: f32,
        sample_rate: f32,
        cutoff: f32,
        resonance: f32,
        drive: f32,
    ) -> f32 {
        let g = libm::tanf(PI * cutoff / sample_rate);
        let g = (g / (1.0 + g)).clamp(0.0, 0.92);
        let feedback = libm::powf(resonance, 1.1) * 3.55;
        let drive_gain = 1.0 + drive * 7.0 + resonance * 1.6;
        let mut stage = diode_curve(input * drive_gain - diode_curve(self.ladder_z[3]) * feedback);

        for z in &mut self.ladder_z {
            let delta = diode_curve(stage - *z) * g;
            *z = sanitize_audio(*z + delta + delta);
            stage = diode_curve(*z);
        }

        sanitize_audio(diode_curve(self.ladder_z[3] * (1.0 + drive * 0.7)))
    }

    #[inline]
    fn process_sem(
        &mut self,
        input: f32,
        sample_rate: f32,
        cutoff: f32,
        resonance: f32,
        drive: f32,
        morph: f32,
    ) -> f32 {
        let g = libm::tanf(PI * cutoff / sample_rate).clamp(0.0, 8.0);
        let damping = (1.85 - resonance * 1.55).max(0.18);
        let driven = fast_tanh(input * (1.0 + drive * 4.0));
        let high = (driven - self.sem_low - damping * self.sem_band) / (1.0 + damping * g + g * g);
        let band = g * high + self.sem_band;
        let low = g * band + self.sem_low;
        self.sem_band = sanitize_audio(g * high + band);
        self.sem_low = sanitize_audio(g * band + low);

        let notch = high + self.sem_low;
        let t = morph * 3.0;
        let output = if t < 1.0 {
            lerp(self.sem_low, self.sem_band, t)
        } else if t < 2.0 {
            lerp(self.sem_band, high, t - 1.0)
        } else {
            lerp(high, notch, t - 2.0)
        };

        sanitize_audio(fast_tanh(output * (1.0 + drive * 0.45)))
    }
}

fn design_biquad(
    mode: BandMode,
    sample_rate: f32,
    frequency_hz: f32,
    gain_db: f32,
    q: f32,
) -> BiquadCoeffs {
    let sample_rate = sample_rate.max(1.0);
    let frequency_hz = frequency_hz.clamp(10.0, sample_rate * 0.49);
    let q = q.max(0.025);
    let omega = 2.0 * PI * frequency_hz / sample_rate;
    let sin_omega = libm::sinf(omega);
    let cos_omega = libm::cosf(omega);
    let alpha = sin_omega / (2.0 * q);
    let amp = libm::powf(10.0, gain_db / 40.0);

    let (b0, b1, b2, a0, a1, a2) = match mode {
        BandMode::Bell => (
            1.0 + alpha * amp,
            -2.0 * cos_omega,
            1.0 - alpha * amp,
            1.0 + alpha / amp,
            -2.0 * cos_omega,
            1.0 - alpha / amp,
        ),
        BandMode::LowShelf => {
            let sqrt_amp = libm::sqrtf(amp);
            let two_sqrt_amp_alpha = 2.0 * sqrt_amp * alpha;
            (
                amp * ((amp + 1.0) - (amp - 1.0) * cos_omega + two_sqrt_amp_alpha),
                2.0 * amp * ((amp - 1.0) - (amp + 1.0) * cos_omega),
                amp * ((amp + 1.0) - (amp - 1.0) * cos_omega - two_sqrt_amp_alpha),
                (amp + 1.0) + (amp - 1.0) * cos_omega + two_sqrt_amp_alpha,
                -2.0 * ((amp - 1.0) + (amp + 1.0) * cos_omega),
                (amp + 1.0) + (amp - 1.0) * cos_omega - two_sqrt_amp_alpha,
            )
        }
        BandMode::HighShelf => {
            let sqrt_amp = libm::sqrtf(amp);
            let two_sqrt_amp_alpha = 2.0 * sqrt_amp * alpha;
            (
                amp * ((amp + 1.0) + (amp - 1.0) * cos_omega + two_sqrt_amp_alpha),
                -2.0 * amp * ((amp - 1.0) + (amp + 1.0) * cos_omega),
                amp * ((amp + 1.0) + (amp - 1.0) * cos_omega - two_sqrt_amp_alpha),
                (amp + 1.0) - (amp - 1.0) * cos_omega + two_sqrt_amp_alpha,
                2.0 * ((amp - 1.0) - (amp + 1.0) * cos_omega),
                (amp + 1.0) - (amp - 1.0) * cos_omega - two_sqrt_amp_alpha,
            )
        }
        BandMode::LowCut => (
            (1.0 + cos_omega) * 0.5,
            -(1.0 + cos_omega),
            (1.0 + cos_omega) * 0.5,
            1.0 + alpha,
            -2.0 * cos_omega,
            1.0 - alpha,
        ),
        BandMode::HighCut => (
            (1.0 - cos_omega) * 0.5,
            1.0 - cos_omega,
            (1.0 - cos_omega) * 0.5,
            1.0 + alpha,
            -2.0 * cos_omega,
            1.0 - alpha,
        ),
        BandMode::Notch => (
            1.0,
            -2.0 * cos_omega,
            1.0,
            1.0 + alpha,
            -2.0 * cos_omega,
            1.0 - alpha,
        ),
        BandMode::BandPass => (
            alpha,
            0.0,
            -alpha,
            1.0 + alpha,
            -2.0 * cos_omega,
            1.0 - alpha,
        ),
    };

    let a0 = a0.max(0.000_001);
    BiquadCoeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

#[inline]
fn coefficient(sample_rate: f32, time_ms: f32) -> f32 {
    let samples = (time_ms * 0.001 * sample_rate).max(1.0);
    libm::expf(-2.0 * PI / samples)
}

#[inline]
fn fast_tanh(input: f32) -> f32 {
    let x = input.clamp(-4.0, 4.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

#[inline]
fn diode_curve(input: f32) -> f32 {
    let shaped = if input >= 0.0 {
        1.0 - libm::expf(-input * 1.72)
    } else {
        -0.7 * (1.0 - libm::expf(input * 2.35))
    };
    fast_tanh(shaped * 1.2)
}

#[inline]
fn soft_clip(sample: f32, amount: f32) -> f32 {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.000_1 {
        return sanitize_audio(sample);
    }
    let drive = 1.0 + amount * 8.0;
    fast_tanh(sample * drive) / fast_tanh(drive)
}

#[inline]
fn sanitize_audio(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-8.0, 8.0)
    } else {
        0.0
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_rises_and_releases() {
        let mut follower = EnvelopeFollower::new(48_000.0);
        follower.configure(48_000.0, 1.0, 50.0);

        let mut level = 0.0;
        for _ in 0..256 {
            level = follower.process(1.0);
        }
        assert!(level > 0.8);

        for _ in 0..256 {
            level = follower.process(0.0);
        }
        assert!(level > 0.0);
        assert!(level < 0.8);
    }

    #[test]
    fn every_master_filter_model_outputs_finite_audio() {
        let mut filter = MasterFilter::new(48_000.0);
        let models = [
            MasterFilterModel::Off,
            MasterFilterModel::CleanLowPass,
            MasterFilterModel::Ladder12,
            MasterFilterModel::Ladder24,
            MasterFilterModel::Diode,
            MasterFilterModel::SemMorph,
        ];

        for model in models {
            filter.reset();
            for sample_idx in 0..512 {
                let input = if sample_idx == 0 { 0.75 } else { 0.0 };
                let (left, right) = filter.process_stereo(
                    input,
                    -input,
                    MasterFilterSettings {
                        model,
                        cutoff_hz: 1_200.0,
                        resonance: 0.7,
                        drive: 0.8,
                        morph: 0.45,
                        mix: 1.0,
                    },
                );
                assert!(left.is_finite());
                assert!(right.is_finite());
            }
        }
    }

    #[test]
    fn every_dynamic_band_mode_outputs_finite_audio() {
        let mut filter = DynamicFilter::new(48_000.0);
        let modes = [
            BandMode::Bell,
            BandMode::LowShelf,
            BandMode::HighShelf,
            BandMode::LowCut,
            BandMode::HighCut,
            BandMode::Notch,
            BandMode::BandPass,
        ];

        for mode in modes {
            let mut settings = DynamicFilterSettings::default();
            settings.bands[0] = DynamicBandSettings {
                enabled: true,
                mode,
                channel_mode: ChannelMode::Stereo,
                frequency_hz: 1_000.0,
                gain_db: 6.0,
                q: 0.707,
                dynamic_db: -12.0,
                sweep_octaves: 1.0,
                env_attack_ms: 1.0,
                env_release_ms: 40.0,
                env_sensitivity: 2.0,
            };

            filter.reset();
            for i in 0..1024 {
                let input = if i == 0 { 1.0 } else { 0.0 };
                let (l, r) = filter.process_stereo(input, -input, &settings);
                assert!(l.is_finite());
                assert!(r.is_finite());
            }
        }
    }

    #[test]
    fn every_dynamic_channel_mode_outputs_finite_audio() {
        let mut filter = DynamicFilter::new(48_000.0);
        let channels = [
            ChannelMode::Stereo,
            ChannelMode::Mid,
            ChannelMode::Side,
            ChannelMode::Left,
            ChannelMode::Right,
        ];

        for channel_mode in channels {
            let mut settings = DynamicFilterSettings::default();
            settings.bands[0] = DynamicBandSettings {
                enabled: true,
                mode: BandMode::Bell,
                channel_mode,
                frequency_hz: 800.0,
                gain_db: 9.0,
                q: 1.2,
                dynamic_db: -18.0,
                sweep_octaves: 0.5,
                env_attack_ms: 2.0,
                env_release_ms: 60.0,
                env_sensitivity: 3.0,
            };

            filter.reset();
            for i in 0..512 {
                let l_in = if i % 2 == 0 { 0.75 } else { -0.25 };
                let r_in = if i % 3 == 0 { -0.5 } else { 0.25 };
                let (l, r) = filter.process_stereo(l_in, r_in, &settings);
                assert!(l.is_finite());
                assert!(r.is_finite());
            }
        }
    }

    #[test]
    fn dynamic_band_responds_to_detector_level() {
        let mut quiet = DynamicFilter::new(48_000.0);
        let mut loud = DynamicFilter::new(48_000.0);
        let mut settings = DynamicFilterSettings::default();
        settings.bands[0] = DynamicBandSettings {
            enabled: true,
            mode: BandMode::Bell,
            channel_mode: ChannelMode::Stereo,
            frequency_hz: 1_200.0,
            gain_db: 0.0,
            q: 0.8,
            dynamic_db: -24.0,
            sweep_octaves: 0.0,
            env_attack_ms: 0.5,
            env_release_ms: 100.0,
            env_sensitivity: 4.0,
        };

        let mut quiet_energy = 0.0;
        let mut loud_energy = 0.0;
        for _ in 0..2048 {
            let (lq, rq) = quiet.process_stereo(0.05, 0.05, &settings);
            let (ll, rl) = loud.process_stereo(0.9, 0.9, &settings);
            quiet_energy += lq.abs() + rq.abs();
            loud_energy += ll.abs() + rl.abs();
        }

        assert!(quiet_energy > 0.0);
        assert!(loud_energy > quiet_energy);
        // The loud path is ducked by the dynamic band, so it should not scale
        // linearly by 18x relative to the quiet input.
        assert!(loud_energy < quiet_energy * 18.0);
    }

    #[test]
    fn dynamic_filter_reports_envelope_activity() {
        let mut filter = DynamicFilter::new(48_000.0);
        let mut settings = DynamicFilterSettings::default();
        settings.bands[0] = DynamicBandSettings {
            enabled: true,
            mode: BandMode::BandPass,
            channel_mode: ChannelMode::Stereo,
            frequency_hz: 900.0,
            gain_db: 3.0,
            q: 1.0,
            dynamic_db: 8.0,
            sweep_octaves: 0.75,
            env_attack_ms: 1.0,
            env_release_ms: 80.0,
            env_sensitivity: 2.0,
        };

        assert_eq!(filter.envelope_values(), [0.0; DYNAMIC_BAND_COUNT]);
        for _ in 0..512 {
            let (left, right) = filter.process_stereo(0.75, -0.75, &settings);
            assert!(left.is_finite());
            assert!(right.is_finite());
        }

        let envelopes = filter.envelope_values();
        assert!(envelopes[0] > 0.0);
        assert!(envelopes[0].is_finite());
        assert_eq!(envelopes[1], 0.0);

        filter.reset();
        assert_eq!(filter.envelope_values(), [0.0; DYNAMIC_BAND_COUNT]);
    }
}
