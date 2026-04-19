//! Isolator-style crossover EQ.
//!
//! Splits the input signal into N frequency bands using Linkwitz-Riley (LR-4)
//! crossover filters with explicit LP/HP pairs, applies independent linear
//! gain to each band, and sums the result. At minimum gain the band
//! multiplier is exactly zero — true silence.
//!
//! Each crossover splits the signal with parallel LP and HP LR-4 filters
//! (24 dB/oct). Allpass compensation on earlier bands ensures flat magnitude
//! sum at unity gains.

use biquad::{Biquad, Coefficients, DirectForm1, Type};
use derivative::Derivative;
use kithara_decode::PcmChunk;

use crate::AudioEffect;

struct Consts;
impl Consts {
    /// Passthrough biquad coefficients (identity filter).
    const PASSTHROUGH: Coefficients<f32> = Coefficients {
        a1: 0.0,
        a2: 0.0,
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
    };

    /// Smoothing time constant in milliseconds.
    const SMOOTH_TIME_MS: f32 = 10.0;

    /// Number of samples between gain-smoothing steps.
    const SMOOTH_BLOCK_SIZE: usize = 32;

    /// Q scaling factor for log-spaced band generation.
    const Q_SCALE_FACTOR: f32 = 1.4;

    /// Reference band count for Q scaling normalization.
    const Q_REFERENCE_BANDS: f32 = 10.0;

    /// Base-10 exponent base for log-spaced frequency calculation.
    const LOG_FREQ_BASE: f32 = 10.0;

    /// Convergence threshold for linear gain smoothing.
    const SMOOTH_CONVERGENCE_THRESHOLD: f32 = 0.0001;

    /// Lowest frequency for log-spaced EQ bands (Hz).
    ///
    /// 60 Hz produces a ~250 Hz low/mid crossover for 3 bands — close to the
    /// industry-standard DJ EQ split point.
    const BAND_MIN_FREQ: f32 = 60.0;

    /// Highest frequency for log-spaced EQ bands (Hz).
    const BAND_MAX_FREQ: f32 = 18000.0;

    /// Milliseconds per second.
    const MS_PER_SEC: f32 = 1000.0;

    /// Minimum channel count for stereo processing.
    const STEREO_CHANNELS: usize = 2;

    /// Butterworth Q factor (1/√2) for LR-4 crossover construction.
    const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

    /// `FilterKind::HighShelf` discriminant in the u8 representation.
    const HIGH_SHELF_DISCRIMINANT: u8 = 2;

    /// Standard dB-to-linear formula divisor: `10^(dB / DB_DIVISOR)`.
    const DB_DIVISOR: f32 = 20.0;

    /// Nyquist normalisation factor for `from_normalized_params`: `2 * f0 / fs`.
    const NYQUIST_FACTOR: f32 = 2.0;

    /// Minimum number of bands that requires crossover filters.
    const MIN_CROSSOVER_BANDS: usize = 2;
}

/// Maximum EQ band gain in dB.
pub const MAX_GAIN_DB: f32 = 6.0;

/// Minimum EQ band gain in dB. At this value the band is fully killed.
pub const MIN_GAIN_DB: f32 = -24.0;

/// The type of biquad filter used for an EQ band.
///
/// Retained for band layout description; the isolator crossover ignores this
/// during processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum FilterKind {
    LowShelf,
    #[default]
    Peaking,
    HighShelf,
}

impl From<u8> for FilterKind {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::LowShelf,
            Consts::HIGH_SHELF_DISCRIMINANT => Self::HighShelf,
            _ => Self::Peaking,
        }
    }
}

/// Configuration for a single EQ band.
#[derive(Debug, Clone, Copy, Derivative, PartialEq)]
#[derivative(Default)]
pub struct EqBandConfig {
    /// Center frequency in Hz.
    #[derivative(Default(value = "1000.0"))]
    pub frequency: f32,
    /// Q factor (used only by `compute_coefficients` for parametric mode).
    #[derivative(Default(value = "std::f32::consts::FRAC_1_SQRT_2"))]
    pub q_factor: f32,
    /// Gain in dB (clamped to [`MIN_GAIN_DB`]..=[`MAX_GAIN_DB`] on set).
    pub gain_db: f32,
    /// Filter type label for this band.
    pub kind: FilterKind,
}

/// Generate logarithmically-spaced EQ bands from 60 Hz to 18 kHz.
///
/// First band is `LowShelf`, last band is `HighShelf`, interior bands are
/// `Peaking`. Q factor scales with band count.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "band count and index are small integers"
)]
pub fn generate_log_spaced_bands(count: usize) -> Vec<EqBandConfig> {
    if count == 0 {
        return Vec::new();
    }

    let q_factor = Consts::Q_SCALE_FACTOR * (count as f32 / Consts::Q_REFERENCE_BANDS).sqrt();

    if count == 1 {
        return vec![EqBandConfig {
            frequency: (Consts::BAND_MIN_FREQ * Consts::BAND_MAX_FREQ).sqrt(),
            q_factor,
            gain_db: 0.0,
            kind: FilterKind::Peaking,
        }];
    }

    let log_min = Consts::BAND_MIN_FREQ.log10();
    let log_max = Consts::BAND_MAX_FREQ.log10();
    let log_step = (log_max - log_min) / (count - 1) as f32;
    let last = count - 1;

    (0..count)
        .map(|i| {
            let kind = if i == 0 {
                FilterKind::LowShelf
            } else if i == last {
                FilterKind::HighShelf
            } else {
                FilterKind::Peaking
            };
            EqBandConfig {
                frequency: Consts::LOG_FREQ_BASE.powf(log_min + i as f32 * log_step),
                q_factor,
                gain_db: 0.0,
                kind,
            }
        })
        .collect()
}

/// Clamp sample to safe range, replacing NaN/Inf with 0.0.
#[inline]
fn clamp_sample(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

/// Convert dB to linear gain. Full kill at [`MIN_GAIN_DB`].
#[inline]
fn db_to_linear(db: f32) -> f32 {
    if db <= MIN_GAIN_DB {
        0.0
    } else {
        Consts::LOG_FREQ_BASE.powf(db / Consts::DB_DIVISOR)
    }
}

/// LR-4 (Linkwitz-Riley 4th-order) filter stage: two cascaded biquads.
struct LR4 {
    bq1: DirectForm1<f32>,
    bq2: DirectForm1<f32>,
}

impl LR4 {
    fn new(coeffs: Coefficients<f32>) -> Self {
        Self {
            bq1: DirectForm1::new(coeffs),
            bq2: DirectForm1::new(coeffs),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.bq2.run(self.bq1.run(input))
    }
}

/// Compute biquad coefficients using correct normalisation.
///
/// `from_params` in biquad 0.5 normalises as `f0/(2·fs)` instead of the
/// Audio EQ Cookbook's `f0/fs`, giving coefficients at 4× lower frequency.
/// We bypass it with `from_normalized_params` and `2·f0/fs`.
fn biquad_coeffs(filter: Type<f32>, freq: f32, sample_rate: f32) -> Coefficients<f32> {
    let normalized = Consts::NYQUIST_FACTOR * freq / sample_rate;
    Coefficients::<f32>::from_normalized_params(filter, normalized, Consts::BUTTERWORTH_Q)
        .unwrap_or(Consts::PASSTHROUGH)
}

/// Per-band gain with linear smoothing.
struct GainState {
    target_db: f32,
    target_linear: f32,
    current_linear: f32,
}

impl GainState {
    fn new(gain_db: f32) -> Self {
        let linear = db_to_linear(gain_db);
        Self {
            target_db: gain_db,
            target_linear: linear,
            current_linear: linear,
        }
    }

    fn set_target(&mut self, gain_db: f32) {
        let clamped = gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        if (clamped - self.target_db).abs() < f32::EPSILON {
            return;
        }
        self.target_db = clamped;
        self.target_linear = db_to_linear(clamped);
    }

    #[inline]
    fn smooth(&mut self, coeff: f32) {
        let diff = self.target_linear - self.current_linear;
        if diff.abs() < Consts::SMOOTH_CONVERGENCE_THRESHOLD {
            self.current_linear = self.target_linear;
        } else {
            self.current_linear += coeff * diff;
        }
    }
}

/// One-pole smoother coefficient, accounting for block-rate updates.
#[expect(
    clippy::cast_precision_loss,
    reason = "Consts::SMOOTH_BLOCK_SIZE is a small constant"
)]
fn compute_smooth_coeff(sample_rate: f32) -> f32 {
    let tau = Consts::SMOOTH_TIME_MS / Consts::MS_PER_SEC;
    let effective_rate = sample_rate / Consts::SMOOTH_BLOCK_SIZE as f32;
    1.0 - (-1.0 / (tau * effective_rate)).exp()
}

/// Single-channel isolator crossover EQ.
///
/// Splits the input into N frequency bands using LR-4 crossover filters
/// with explicit LP/HP pairs and allpass phase compensation. At
/// [`MIN_GAIN_DB`] the band multiplier is zero — true silence.
///
/// ## Crossover topology (3 bands, 2 crossovers at f₁, f₂)
///
/// ```text
/// input → LP₁ → AP₂ → ×gain₀ ──┐
///       → HP₁ → LP₂ → ×gain₁ ──┼─► Σ → output
///       → HP₁ → HP₂ → ×gain₂ ──┘
/// ```
///
/// The allpass at f₂ on the low band compensates for the phase shift of
/// the second crossover, so LP + HP sums to a flat allpass at unity gains.
pub struct IsolatorEq {
    /// N-1 LR-4 lowpass stages (one per crossover).
    lps: Vec<LR4>,
    /// N-1 LR-4 highpass stages (one per crossover).
    hps: Vec<LR4>,
    /// Flattened allpass compensation filters (contiguous for cache locality).
    /// Band k's allpasses are at `ap_filters[ap_offsets[k]..ap_offsets[k+1]]`.
    ap_filters: Vec<DirectForm1<f32>>,
    /// Start offset per band into `ap_filters` (length = N+1).
    ap_offsets: Vec<usize>,
    /// Scratch buffer for LP outputs (pre-allocated, size N-1).
    lp_scratch: Vec<f32>,
    /// Per-band gain state.
    gains: Vec<GainState>,
    /// Crossover frequencies (N-1).
    crossover_freqs: Vec<f32>,
    sample_rate: f32,
    smooth_coeff: f32,
    block_counter: usize,
    /// Recent-input ring buffer used to rehydrate filter state when exiting
    /// a fast path (unity bypass or full silence). Size covers the LR-4 +
    /// allpass cascade settle time for the lowest default crossover.
    bypass_history: Vec<f32>,
    /// Write position in [`Self::bypass_history`] (modulo its length).
    bypass_history_pos: usize,
    /// Whether the previous sample returned via a fast path and therefore
    /// left the filter state frozen — drives a one-shot rehydration on the
    /// next non-fast-path sample.
    was_in_fastpath: bool,
}

impl IsolatorEq {
    /// Create a new isolator EQ for the given band layout.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    pub fn new(bands: &[EqBandConfig], sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let n = bands.len();
        let xover_count = n.saturating_sub(1);

        let crossover_freqs: Vec<f32> = if n >= Consts::MIN_CROSSOVER_BANDS {
            (0..xover_count)
                .map(|i| (bands[i].frequency * bands[i + 1].frequency).sqrt())
                .collect()
        } else {
            Vec::new()
        };

        let lps: Vec<LR4> = crossover_freqs
            .iter()
            .map(|&f| LR4::new(biquad_coeffs(Type::LowPass, f, sr)))
            .collect();

        let hps: Vec<LR4> = crossover_freqs
            .iter()
            .map(|&f| LR4::new(biquad_coeffs(Type::HighPass, f, sr)))
            .collect();

        // Build flattened allpass array. Band k needs allpasses at
        // crossover frequencies k+1 .. N-2 (LR-4 LP+HP = 2nd-order allpass).
        let mut ap_filters = Vec::new();
        let mut ap_offsets = Vec::with_capacity(n + 1);
        for band in 0..n {
            ap_offsets.push(ap_filters.len());
            if band + 1 < xover_count {
                for &f in &crossover_freqs[band + 1..] {
                    ap_filters.push(DirectForm1::new(biquad_coeffs(Type::AllPass, f, sr)));
                }
            }
        }
        ap_offsets.push(ap_filters.len());

        let gains = bands.iter().map(|b| GainState::new(b.gain_db)).collect();

        Self {
            lps,
            hps,
            ap_filters,
            ap_offsets,
            lp_scratch: vec![0.0; xover_count],
            gains,
            crossover_freqs,
            sample_rate: sr,
            smooth_coeff: compute_smooth_coeff(sr),
            block_counter: 0,
            bypass_history: vec![0.0; Self::BYPASS_HISTORY_LEN],
            bypass_history_pos: 0,
            was_in_fastpath: false,
        }
    }

    /// Size of the fast-path rehydration ring buffer. 128 samples covers
    /// the LR-4 + allpass cascade settle time for the lowest default
    /// crossover (~250 Hz at 44.1 kHz).
    const BYPASS_HISTORY_LEN: usize = 128;

    /// Set the target gain for a band (dB, clamped to min/max).
    pub fn set_gain(&mut self, band: usize, gain_db: f32) {
        if let Some(state) = self.gains.get_mut(band) {
            state.set_target(gain_db);
        }
    }

    /// Current target gain for a band (dB).
    #[must_use]
    pub fn target_gain(&self, band: usize) -> Option<f32> {
        self.gains.get(band).map(|s| s.target_db)
    }

    /// Number of bands.
    #[must_use]
    pub fn band_count(&self) -> usize {
        self.gains.len()
    }

    /// Whether any band is still smoothing toward its target.
    #[must_use]
    pub fn is_smoothing(&self) -> bool {
        self.gains.iter().any(|g| {
            (g.target_linear - g.current_linear).abs() > Consts::SMOOTH_CONVERGENCE_THRESHOLD
        })
    }

    /// Whether every band sits at exactly unity gain with no smoothing in
    /// flight — in that case `process_sample` can skip the LR-4 + allpass
    /// chain entirely and return the input unchanged.
    ///
    /// Instruments traces show `process_sample` as ~3 % of total CPU during
    /// HLS playback; gating the filter chain on non-neutral gain eliminates
    /// that cost for the common case of a user who never touches the EQ.
    #[must_use]
    pub fn is_bypass_active(&self) -> bool {
        !self.gains.is_empty()
            && self.gains.iter().all(|g| {
                (g.target_linear - 1.0).abs() < f32::EPSILON
                    && (g.current_linear - 1.0).abs() < f32::EPSILON
            })
    }

    /// Whether every band is at full kill (linear 0) with no smoothing in
    /// flight — in that case `process_sample` can skip the LR-4 + allpass
    /// chain entirely and return a literal zero.
    ///
    /// Mirrors [`Self::is_bypass_active`] for the opposite extreme: a user
    /// who slammed every fader to [`MIN_GAIN_DB`] wants silence, not a
    /// near-zero filtered signal.
    #[must_use]
    pub fn is_silence_active(&self) -> bool {
        !self.gains.is_empty()
            && self.gains.iter().all(|g| {
                g.target_linear.abs() < f32::EPSILON && g.current_linear.abs() < f32::EPSILON
            })
    }

    fn record_bypass_input(&mut self, input: f32) {
        let len = self.bypass_history.len();
        self.bypass_history[self.bypass_history_pos] = input;
        self.bypass_history_pos = (self.bypass_history_pos + 1) % len;
    }

    /// Replay the ring-buffered recent input through the filter cascade so
    /// the next real sample sees filter state consistent with the signal,
    /// not the frozen state from before the fast path was entered.
    ///
    /// Writes into the exact same filter cascade as the main path but
    /// discards the output — this is purely for filter-state hydration.
    fn rehydrate_filter_state(&mut self) {
        let n = self.gains.len();
        if n < Consts::MIN_CROSSOVER_BANDS {
            return;
        }
        let len = self.bypass_history.len();
        let start = self.bypass_history_pos;
        for offset in 0..len {
            let idx = (start + offset) % len;
            let s = self.bypass_history[idx];
            let mut hp = s;
            for i in 0..n - 1 {
                self.lp_scratch[i] = self.lps[i].process(hp);
                hp = self.hps[i].process(hp);
            }
            for i in 0..n - 1 {
                let mut band = self.lp_scratch[i];
                let ap_start = self.ap_offsets[i];
                let ap_end = self.ap_offsets[i + 1];
                for ap in &mut self.ap_filters[ap_start..ap_end] {
                    band = ap.run(band);
                }
            }
        }
    }

    /// Process a single sample through the crossover EQ.
    ///
    /// Gain smoothing advances automatically every [`Consts::SMOOTH_BLOCK_SIZE`] calls.
    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        self.block_counter += 1;
        if self.block_counter >= Consts::SMOOTH_BLOCK_SIZE {
            self.block_counter = 0;
            for gain in &mut self.gains {
                gain.smooth(self.smooth_coeff);
            }
        }

        // Fast paths skip the LR-4 + allpass chain when the result is
        // trivially known. Filter state freezes while a fast path is active
        // and is rehydrated from a ring buffer on exit, so resuming filter
        // work doesn't jump from zero-state (see rehydrate_filter_state).
        if self.is_silence_active() {
            self.record_bypass_input(input);
            self.was_in_fastpath = true;
            return 0.0;
        }
        if self.is_bypass_active() {
            self.record_bypass_input(input);
            self.was_in_fastpath = true;
            return input;
        }
        if self.was_in_fastpath {
            self.was_in_fastpath = false;
            self.rehydrate_filter_state();
        }

        let n = self.gains.len();
        if n == 0 {
            return input;
        }
        if n == 1 {
            return clamp_sample(input * self.gains[0].current_linear);
        }

        // Split via LP/HP chain: each crossover peels off a band via LP,
        // and the HP output feeds the next crossover.
        let mut hp = input;
        for i in 0..n - 1 {
            self.lp_scratch[i] = self.lps[i].process(hp);
            hp = self.hps[i].process(hp);
        }

        // Accumulate: each LP band runs through its allpass compensation,
        // then is scaled by the band gain.
        let mut output = 0.0;
        for i in 0..n - 1 {
            let mut band = self.lp_scratch[i];
            let ap_start = self.ap_offsets[i];
            let ap_end = self.ap_offsets[i + 1];
            for ap in &mut self.ap_filters[ap_start..ap_end] {
                band = ap.run(band);
            }
            output += band * self.gains[i].current_linear;
        }
        // Last band is the final HP remainder.
        output += hp * self.gains[n - 1].current_linear;

        clamp_sample(output)
    }

    /// Reset all gains to 0 dB (unity) and clear filter state.
    pub fn reset(&mut self) {
        for gain in &mut self.gains {
            gain.target_db = 0.0;
            gain.target_linear = 1.0;
            gain.current_linear = 1.0;
        }
        self.rebuild_filters();
        self.block_counter = 0;
        self.bypass_history.fill(0.0);
        self.bypass_history_pos = 0;
        self.was_in_fastpath = false;
    }

    /// Re-initialise for a new sample rate (e.g. after stream change).
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    pub fn update_sample_rate(&mut self, sample_rate: u32) {
        self.sample_rate = sample_rate as f32;
        self.smooth_coeff = compute_smooth_coeff(self.sample_rate);
        self.rebuild_filters();
    }

    fn rebuild_filters(&mut self) {
        for (i, &freq) in self.crossover_freqs.iter().enumerate() {
            self.lps[i] = LR4::new(biquad_coeffs(Type::LowPass, freq, self.sample_rate));
            self.hps[i] = LR4::new(biquad_coeffs(Type::HighPass, freq, self.sample_rate));
        }
        for band in 0..self.gains.len() {
            let start = self.ap_offsets[band];
            let end = self.ap_offsets[band + 1];
            for (j, ap) in self.ap_filters[start..end].iter_mut().enumerate() {
                let freq = self.crossover_freqs[band + 1 + j];
                *ap = DirectForm1::new(biquad_coeffs(Type::AllPass, freq, self.sample_rate));
            }
        }
    }
}

/// N-band isolator EQ implementing [`AudioEffect`].
///
/// Wraps two [`IsolatorEq`] instances (L/R) and processes interleaved PCM.
pub struct EqEffect {
    eq_l: IsolatorEq,
    eq_r: IsolatorEq,
    bands: Vec<EqBandConfig>,
    channels: u16,
}

impl EqEffect {
    /// Create a new EQ effect with the given bands and audio format.
    #[must_use]
    pub fn new(bands: Vec<EqBandConfig>, sample_rate: u32, channels: u16) -> Self {
        let eq_l = IsolatorEq::new(&bands, sample_rate);
        let eq_r = IsolatorEq::new(&bands, sample_rate);
        Self {
            eq_l,
            eq_r,
            bands,
            channels,
        }
    }

    /// Set the gain for a specific band (clamped to min/max dB).
    pub fn set_gain(&mut self, band_index: usize, gain_db: f32) {
        self.eq_l.set_gain(band_index, gain_db);
        self.eq_r.set_gain(band_index, gain_db);
    }

    /// Get the band layout. Gains reflect target values.
    #[must_use]
    pub fn bands(&self) -> Vec<EqBandConfig> {
        self.bands
            .iter()
            .enumerate()
            .map(|(i, band)| EqBandConfig {
                gain_db: self.eq_l.target_gain(i).unwrap_or(0.0),
                ..*band
            })
            .collect()
    }

    /// Get the target gain for a specific band.
    #[must_use]
    pub fn target_gain(&self, band_index: usize) -> Option<f32> {
        self.eq_l.target_gain(band_index)
    }

    /// Check if any band is currently smoothing.
    #[cfg(test)]
    fn is_smoothing(&self) -> bool {
        self.eq_l.is_smoothing()
    }
}

impl AudioEffect for EqEffect {
    fn process(&mut self, mut chunk: PcmChunk) -> Option<PcmChunk> {
        let channels = self.channels as usize;
        if channels == 0 {
            return Some(chunk);
        }

        let samples = chunk.pcm.as_mut_slice();

        for frame in samples.chunks_exact_mut(channels) {
            frame[0] = self.eq_l.process_sample(frame[0]);
            if channels >= Consts::STEREO_CHANNELS {
                frame[1] = self.eq_r.process_sample(frame[1]);
            }
        }

        Some(chunk)
    }

    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn reset(&mut self) {
        self.eq_l.reset();
        self.eq_r.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use kithara_bufpool::pcm_pool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;

    use super::*;

    fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    #[kithara::test]
    #[case(0, 0)]
    #[case(1, 1)]
    #[case(10, 10)]
    fn generates_log_spaced_bands(#[case] count: usize, #[case] expected_len: usize) {
        let bands = generate_log_spaced_bands(count);
        assert_eq!(bands.len(), expected_len);

        match count {
            0 => assert!(bands.is_empty()),
            1 => {
                let expected = (Consts::BAND_MIN_FREQ * Consts::BAND_MAX_FREQ).sqrt();
                assert!((bands[0].frequency - expected).abs() < 1.0);
            }
            10 => {
                assert!((bands[0].frequency - Consts::BAND_MIN_FREQ).abs() < 1.0);
                assert!((bands[9].frequency - 18000.0).abs() < 1.0);
                for pair in bands.windows(2) {
                    assert!(pair[1].frequency > pair[0].frequency);
                }
                for band in &bands {
                    assert!((band.gain_db).abs() < f32::EPSILON);
                }
            }
            _ => {}
        }
    }

    #[kithara::test]
    fn generate_bands_3_first_is_low_shelf() {
        let bands = generate_log_spaced_bands(3);
        assert_eq!(bands[0].kind, FilterKind::LowShelf);
    }

    #[kithara::test]
    fn generate_bands_3_last_is_high_shelf() {
        let bands = generate_log_spaced_bands(3);
        assert_eq!(bands[2].kind, FilterKind::HighShelf);
    }

    #[kithara::test]
    fn generate_bands_3_mid_is_peaking() {
        let bands = generate_log_spaced_bands(3);
        assert_eq!(bands[1].kind, FilterKind::Peaking);
    }

    #[kithara::test]
    fn generate_bands_10_edges_are_shelves() {
        let bands = generate_log_spaced_bands(10);
        assert_eq!(bands[0].kind, FilterKind::LowShelf);
        assert_eq!(bands[9].kind, FilterKind::HighShelf);
        for band in &bands[1..9] {
            assert_eq!(band.kind, FilterKind::Peaking);
        }
    }

    #[kithara::test]
    fn filter_kind_default_is_peaking() {
        assert_eq!(FilterKind::default(), FilterKind::Peaking);
    }

    #[kithara::test]
    fn eq_band_config_has_filter_kind() {
        let band = EqBandConfig::default();
        assert_eq!(band.kind, FilterKind::Peaking);
    }

    #[kithara::test]
    fn three_band_crossover_near_250_and_2500() {
        let bands = generate_log_spaced_bands(3);
        let xover_low = (bands[0].frequency * bands[1].frequency).sqrt();
        let xover_high = (bands[1].frequency * bands[2].frequency).sqrt();

        assert!(
            (200.0..350.0).contains(&xover_low),
            "low crossover {xover_low:.0} Hz should be near 250 Hz"
        );
        assert!(
            (2000.0..6000.0).contains(&xover_high),
            "high crossover {xover_high:.0} Hz should be near 2500 Hz"
        );
    }

    #[kithara::test]
    fn eq_flat_gain_preserves_magnitude() {
        let bands = generate_log_spaced_bands(10);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        // Process enough audio for the crossover filters to settle.
        let warmup = vec![0.0f32; 4096];
        let _ = eq.process(test_chunk(spec, warmup));

        let num_frames = 44100;
        let pcm: Vec<f32> = (0..num_frames)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / 44100.0).sin())
            .collect();

        let input_rms: f32 = (pcm.iter().map(|s| s * s).sum::<f32>() / num_frames as f32).sqrt();

        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();
        let out = output.samples();

        // Skip transient, measure steady-state RMS.
        let steady = &out[4096..];
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();
        let gain = output_rms / input_rms;

        // At unity gains the crossover is an allpass (magnitude 1).
        assert!(
            (gain - 1.0).abs() < 0.05,
            "Unity gain should preserve magnitude, got gain={gain:.4}"
        );
    }

    #[kithara::test]
    fn eq_set_gain_clamps() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        eq.set_gain(0, 100.0);
        assert!((eq.target_gain(0).unwrap() - MAX_GAIN_DB).abs() < f32::EPSILON);

        eq.set_gain(0, -100.0);
        assert!((eq.target_gain(0).unwrap() - MIN_GAIN_DB).abs() < f32::EPSILON);

        eq.set_gain(0, 3.0);
        assert!((eq.target_gain(0).unwrap() - 3.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn eq_set_gain_out_of_bounds_band_is_noop() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);
        eq.set_gain(99, 5.0);
        for i in 0..3 {
            assert!(eq.target_gain(i).unwrap().abs() < f32::EPSILON);
        }
    }

    #[kithara::test]
    fn eq_reset_clears_gains_and_history() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        eq.set_gain(0, 6.0);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.5f32; 256];
        let chunk = test_chunk(spec, pcm);
        let _ = eq.process(chunk);

        eq.reset();

        for i in 0..3 {
            assert!(
                eq.target_gain(i).unwrap().abs() < f32::EPSILON,
                "target should be 0 after reset"
            );
        }
    }

    #[kithara::test]
    fn eq_single_band_kill() {
        let bands = vec![EqBandConfig {
            frequency: 1000.0,
            ..Default::default()
        }];
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(0, MIN_GAIN_DB);
        converge_smoother(&mut eq, spec);

        let gain = measure_sine_gain(&mut eq, 1000.0, spec);
        assert!(
            gain < 0.001,
            "single band at min should be killed, got gain={gain:.6}"
        );
    }

    #[kithara::test]
    fn eq_3band_kill_low() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(0, MIN_GAIN_DB);
        converge_smoother(&mut eq, spec);

        let gain_bass = measure_sine_gain(&mut eq, 40.0, spec);
        let gain_treble = measure_sine_gain(&mut eq, 10000.0, spec);
        assert!(
            gain_bass < 0.05,
            "bass should be killed, got {gain_bass:.4}"
        );
        assert!(
            gain_treble > 0.8,
            "treble should pass, got {gain_treble:.4}"
        );
    }

    #[kithara::test]
    fn eq_3band_kill_high() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(2, MIN_GAIN_DB);
        converge_smoother(&mut eq, spec);

        let gain_treble = measure_sine_gain(&mut eq, 15000.0, spec);
        let gain_bass = measure_sine_gain(&mut eq, 40.0, spec);
        assert!(
            gain_treble < 0.05,
            "treble should be killed, got {gain_treble:.4}"
        );
        assert!(gain_bass > 0.8, "bass should pass, got {gain_bass:.4}");
    }

    #[kithara::test]
    fn eq_3band_kill_all_produces_silence() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        for i in 0..3 {
            eq.set_gain(i, MIN_GAIN_DB);
        }
        converge_smoother(&mut eq, spec);

        for freq in [40.0, 1000.0, 10000.0] {
            let gain = measure_sine_gain(&mut eq, freq, spec);
            assert!(
                gain < 0.001,
                "all bands killed: {freq}Hz gain should be ~0, got {gain:.6}"
            );
        }
    }

    #[kithara::test]
    fn eq_low_shelf_boosts_bass() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(0, MAX_GAIN_DB);
        converge_smoother(&mut eq, spec);

        let gain_bass = measure_sine_gain(&mut eq, 40.0, spec);
        assert!(
            gain_bass > 1.5,
            "40Hz should be boosted, got gain={gain_bass:.3}"
        );
    }

    #[kithara::test]
    fn eq_high_shelf_boosts_treble() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(2, MAX_GAIN_DB);
        converge_smoother(&mut eq, spec);

        let gain_treble = measure_sine_gain(&mut eq, 15000.0, spec);
        assert!(
            gain_treble > 1.5,
            "15kHz should be boosted, got gain={gain_treble:.3}"
        );
    }

    #[kithara::test]
    fn eq_gain_change_starts_smoothing() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        assert!(!eq.is_smoothing(), "should not be smoothing initially");
        eq.set_gain(0, 6.0);
        assert!(eq.is_smoothing(), "should be smoothing after set_gain");
    }

    #[kithara::test]
    fn eq_smooth_gain_converges() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(0, 6.0);

        converge_smoother(&mut eq, spec);

        assert!(
            !eq.is_smoothing(),
            "should have converged after sufficient processing"
        );
    }

    #[kithara::test]
    fn eq_smooth_no_discontinuity() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        // Warmup
        let warmup: Vec<f32> = (0..4096)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, warmup);
        let _ = eq.process(chunk);

        // Abrupt gain change
        eq.set_gain(0, MAX_GAIN_DB);

        let signal: Vec<f32> = (0..4096)
            .map(|i| (2.0 * PI * 1000.0 * (i + 4096) as f32 / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, signal);
        let output = eq.process(chunk).unwrap();
        let out = output.samples();

        let max_diff = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);

        assert!(
            max_diff < 0.5,
            "Discontinuity detected: max sample diff = {max_diff:.4}"
        );
    }

    #[kithara::test]
    #[case(2, 256, Some((2, 3.0)))]
    #[case(1, 128, None)]
    fn eq_process_supported_channel_layouts(
        #[case] channels: u16,
        #[case] sample_len: usize,
        #[case] gain: Option<(usize, f32)>,
    ) {
        let bands = generate_log_spaced_bands(5);
        let spec = PcmSpec {
            channels,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        if let Some((band, gain_db)) = gain {
            eq.set_gain(band, gain_db);
        }

        let pcm = vec![0.5f32; sample_len];
        let chunk = test_chunk(spec, pcm);
        let result = eq.process(chunk);
        assert!(result.is_some());
        assert_eq!(result.unwrap().samples().len(), sample_len);
    }

    #[kithara::test]
    fn eq_flush_returns_none() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);
        assert!(eq.flush().is_none());
    }

    #[kithara::test]
    fn eq_output_never_nan_or_inf() {
        let bands = generate_log_spaced_bands(10);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        for round in 0..100 {
            let gain = if round % 2 == 0 {
                MAX_GAIN_DB
            } else {
                MIN_GAIN_DB
            };
            for band in 0..10 {
                eq.set_gain(band, gain);
            }

            let pcm: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.1).sin()).collect();
            let chunk = test_chunk(spec, pcm);
            let output = eq.process(chunk).unwrap();
            for (i, &s) in output.samples().iter().enumerate() {
                assert!(s.is_finite(), "round {round} sample {i}: got {s}");
            }
        }
    }

    #[kithara::test]
    fn eq_nan_input_produces_safe_output() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);
        eq.set_gain(0, 6.0);
        converge_smoother(&mut eq, spec);

        let mut pcm = vec![0.5f32; 256];
        pcm[10] = f32::NAN;
        pcm[20] = f32::INFINITY;
        pcm[30] = f32::NEG_INFINITY;
        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();

        for (i, &s) in output.samples().iter().enumerate() {
            assert!(s.is_finite(), "sample {i}: got {s}");
        }
    }

    #[kithara::test]
    fn eq_extreme_gain_oscillation_stays_safe() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        for round in 0..200 {
            let gain = if round % 2 == 0 {
                MAX_GAIN_DB
            } else {
                MIN_GAIN_DB
            };
            eq.set_gain(0, gain);
            eq.set_gain(1, -gain);
            eq.set_gain(2, gain);

            let pcm: Vec<f32> = (0..512).map(|i| ((i as f32) * 0.3).sin()).collect();
            let chunk = test_chunk(spec, pcm);
            let output = eq.process(chunk).unwrap();
            for &s in output.samples() {
                assert!(s.is_finite());
            }
        }
    }

    #[kithara::test]
    fn butterworth_lp_dc_gain_is_unity() {
        let coeffs = biquad_coeffs(Type::LowPass, 250.0, 44100.0);
        let dc_gain = (coeffs.b0 + coeffs.b1 + coeffs.b2) / (1.0 + coeffs.a1 + coeffs.a2);
        assert!(
            (dc_gain - 1.0).abs() < 0.001,
            "LP DC gain should be 1.0, got {dc_gain}"
        );
    }

    #[kithara::test]
    fn db_to_linear_kill_at_min() {
        assert!((db_to_linear(MIN_GAIN_DB)).abs() < f32::EPSILON);
        assert!((db_to_linear(-30.0)).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn db_to_linear_unity_at_zero() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 0.001);
    }

    #[kithara::test]
    fn db_to_linear_boost_at_6db() {
        let gain = db_to_linear(6.0);
        assert!((gain - 2.0).abs() < 0.02, "+6dB should be ~2.0, got {gain}");
    }

    #[kithara::test]
    fn eq_fresh_at_zero_db_is_bypass_active() {
        let bands = generate_log_spaced_bands(3);
        let eq = IsolatorEq::new(&bands, 44100);
        assert!(
            eq.is_bypass_active(),
            "default 0 dB bands should activate bypass so the LR-4 chain \
             never runs for users who never touch the EQ"
        );
    }

    #[kithara::test]
    fn eq_bypass_deactivates_on_gain_change() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        assert!(eq.is_bypass_active(), "precondition: fresh EQ is in bypass");

        eq.set_gain(0, 3.0);

        assert!(
            !eq.is_bypass_active(),
            "bypass must deactivate the instant any band targets a non-unity \
             gain, so the next sample reaches the actual filter chain"
        );
    }

    #[kithara::test]
    fn eq_bypass_reactivates_after_return_to_unity() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq_effect = EqEffect::new(bands, spec.sample_rate, spec.channels);

        eq_effect.set_gain(0, 6.0);
        converge_smoother(&mut eq_effect, spec);
        assert!(!eq_effect.eq_l.is_bypass_active());

        eq_effect.set_gain(0, 0.0);
        converge_smoother(&mut eq_effect, spec);
        converge_smoother(&mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.is_bypass_active(),
            "after gains smooth back to unity, bypass must reactivate so the \
             filter chain stops running"
        );
    }

    #[kithara::test]
    fn eq_bypass_returns_input_unchanged() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        assert!(eq.is_bypass_active(), "precondition: bypass is active");

        let inputs = [0.0_f32, 0.25, -0.5, 0.999, -0.999, 1e-6, -1e-6];
        for &input in &inputs {
            let output = eq.process_sample(input);
            assert_eq!(
                output, input,
                "bypass must return input bit-for-bit, got {output} for {input}"
            );
        }
    }

    #[kithara::test]
    fn eq_all_min_gain_after_smoothing_is_silence_active() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq_effect = EqEffect::new(bands, spec.sample_rate, spec.channels);

        for i in 0..3 {
            eq_effect.set_gain(i, MIN_GAIN_DB);
        }
        converge_smoother(&mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.is_silence_active(),
            "all bands at MIN_GAIN_DB after smoother converges must activate \
             the silence fast path so the filter chain is skipped entirely"
        );
    }

    #[kithara::test]
    fn eq_silence_returns_zero() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        for i in 0..3 {
            eq.gains[i].target_linear = 0.0;
            eq.gains[i].current_linear = 0.0;
            eq.gains[i].target_db = MIN_GAIN_DB;
        }
        assert!(eq.is_silence_active(), "precondition: silence is active");

        let inputs = [0.0_f32, 0.25, -0.5, 0.999, -0.999];
        for &input in &inputs {
            let output = eq.process_sample(input);
            assert_eq!(
                output, 0.0,
                "silence must return literal 0.0 for any input, got {output} \
                 for {input}"
            );
        }
    }

    #[kithara::test]
    fn eq_silence_deactivates_when_any_band_raised() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        for i in 0..3 {
            eq.gains[i].target_linear = 0.0;
            eq.gains[i].current_linear = 0.0;
            eq.gains[i].target_db = MIN_GAIN_DB;
        }
        assert!(eq.is_silence_active(), "precondition: silence is active");

        eq.set_gain(1, -3.0);

        assert!(
            !eq.is_silence_active(),
            "raising any band above MIN_GAIN_DB must disable silence so the \
             filter chain re-engages via smoother ramp-up"
        );
    }

    fn converge_smoother(eq: &mut EqEffect, spec: PcmSpec) {
        let frames = (spec.sample_rate as usize) / 5;
        let pcm = vec![0.0f32; frames * spec.channels as usize];
        let chunk = test_chunk(spec, pcm);
        let _ = eq.process(chunk);
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "frame count and index are small integers"
    )]
    fn measure_sine_gain(eq: &mut EqEffect, freq_hz: f32, spec: PcmSpec) -> f32 {
        let num_frames = 44100;
        let mut pcm = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            let sample = (2.0 * PI * freq_hz * i as f32 / spec.sample_rate as f32).sin();
            pcm.push(sample);
        }

        let input_rms: f32 = (pcm.iter().map(|s| s * s).sum::<f32>() / num_frames as f32).sqrt();

        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();
        let out = output.samples();

        let steady = &out[4096..];
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();

        output_rms / input_rms
    }
}
