//! Parametric EQ effect using biquad filter cascade.
//!
//! Provides an N-band parametric equalizer implementing [`AudioEffect`].
//! Each band uses a configurable biquad filter (low-shelf, peaking, or high-shelf)
//! per channel, applied as a cascade. Gain changes are smoothed per-block using a
//! one-pole exponential smoother to avoid clicks and pops.

use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type};
use derivative::Derivative;
use kithara_decode::PcmChunk;

use crate::AudioEffect;

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

/// Number of samples between coefficient recalculations during smoothing.
const SMOOTH_BLOCK_SIZE: usize = 32;

/// Q scaling factor for log-spaced band generation.
const Q_SCALE_FACTOR: f32 = 1.4;

/// Reference band count for Q scaling normalization.
const Q_REFERENCE_BANDS: f32 = 10.0;

/// Base-10 exponent base for log-spaced frequency calculation.
const LOG_FREQ_BASE: f32 = 10.0;

/// Minimum gain threshold (dB) below which the filter is treated as passthrough.
const GAIN_PASSTHROUGH_THRESHOLD: f32 = 0.01;

/// Maximum EQ band gain in dB.
const MAX_GAIN_DB: f32 = 6.0;

/// Minimum EQ band gain in dB.
const MIN_GAIN_DB: f32 = -24.0;

/// Convergence threshold for gain smoothing (dB).
const SMOOTH_CONVERGENCE_THRESHOLD: f32 = 0.001;

/// Lowest frequency for log-spaced EQ bands (Hz).
const BAND_MIN_FREQ: f32 = 30.0;

/// Highest frequency for log-spaced EQ bands (Hz).
const BAND_MAX_FREQ: f32 = 18000.0;

/// Milliseconds per second for time constant conversion.
const MS_PER_SEC: f32 = 1000.0;

/// Minimum channel count for stereo processing.
const STEREO_CHANNELS: usize = 2;

/// The type of biquad filter used for an EQ band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum FilterKind {
    /// Low-shelf filter: boosts/cuts frequencies below the center frequency.
    LowShelf,
    /// Peaking EQ filter: boosts/cuts frequencies around the center frequency.
    #[default]
    Peaking,
    /// High-shelf filter: boosts/cuts frequencies above the center frequency.
    HighShelf,
}

/// Configuration for a single parametric EQ band.
#[derive(Debug, Clone, Copy, Derivative, PartialEq)]
#[derivative(Default)]
pub struct EqBandConfig {
    /// Center frequency in Hz.
    #[derivative(Default(value = "1000.0"))]
    pub frequency: f32,
    /// Q factor (bandwidth control).
    #[derivative(Default(value = "std::f32::consts::FRAC_1_SQRT_2"))]
    pub q_factor: f32,
    /// Gain in dB (clamped to -24..+6 on set).
    pub gain_db: f32,
    /// Filter type for this band.
    pub kind: FilterKind,
}

/// Generate logarithmically-spaced EQ bands from 30 Hz to 18 kHz.
///
/// First band is `LowShelf`, last band is `HighShelf`, interior bands are `Peaking`.
/// Q factor is scaled by `1.4 * sqrt(count / 10)` to provide narrower bands
/// when there are more of them. For a single band, the geometric mean frequency
/// of 30 Hz and 18 kHz is used with `Peaking` kind.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "band count and index are small integers"
)]
pub fn generate_log_spaced_bands(count: usize) -> Vec<EqBandConfig> {
    if count == 0 {
        return Vec::new();
    }

    let q_factor = Q_SCALE_FACTOR * (count as f32 / Q_REFERENCE_BANDS).sqrt();

    if count == 1 {
        return vec![EqBandConfig {
            frequency: (BAND_MIN_FREQ * BAND_MAX_FREQ).sqrt(),
            q_factor,
            gain_db: 0.0,
            kind: FilterKind::Peaking,
        }];
    }

    let log_min = BAND_MIN_FREQ.log10();
    let log_max = BAND_MAX_FREQ.log10();
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
                frequency: LOG_FREQ_BASE.powf(log_min + i as f32 * log_step),
                q_factor,
                gain_db: 0.0,
                kind,
            }
        })
        .collect()
}

/// Compute biquad coefficients for a band with the given gain.
#[must_use]
pub fn compute_coefficients(
    band: &EqBandConfig,
    gain_db: f32,
    sample_rate: biquad::Hertz<f32>,
) -> Coefficients<f32> {
    if gain_db.abs() < GAIN_PASSTHROUGH_THRESHOLD {
        return PASSTHROUGH;
    }
    let filter_type = match band.kind {
        FilterKind::LowShelf => Type::LowShelf(gain_db),
        FilterKind::Peaking => Type::PeakingEQ(gain_db),
        FilterKind::HighShelf => Type::HighShelf(gain_db),
    };
    Coefficients::<f32>::from_params(filter_type, sample_rate, band.frequency.hz(), band.q_factor)
        .unwrap_or(PASSTHROUGH)
}

/// Per-band smoother state for click-free gain transitions.
struct BandState {
    /// Target gain set by the user (clamped).
    target_gain_db: f32,
    /// Current smoothed gain applied to the filter.
    current_gain_db: f32,
}

/// Parametric EQ effect using biquad filter cascade.
///
/// Applies N configurable biquad filters (low-shelf, peaking, high-shelf) in series
/// per channel. Gain changes are smoothed using a one-pole exponential filter to
/// prevent clicks and pops during real-time parameter updates.
pub struct EqEffect {
    bands: Vec<EqBandConfig>,
    states: Vec<BandState>,
    filters_l: Vec<DirectForm1<f32>>,
    filters_r: Vec<DirectForm1<f32>>,
    sample_rate: u32,
    channels: u16,
    /// One-pole smoother coefficient: `1 - exp(-1 / (tau * fs))`.
    smooth_coeff: f32,
}

impl EqEffect {
    /// Create a new EQ effect with the given bands and audio format.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    pub fn new(bands: Vec<EqBandConfig>, sample_rate: u32, channels: u16) -> Self {
        let n = bands.len();
        let states: Vec<BandState> = bands
            .iter()
            .map(|b| BandState {
                target_gain_db: b.gain_db,
                current_gain_db: b.gain_db,
            })
            .collect();
        let smooth_coeff = Self::compute_smooth_coeff(sample_rate as f32);
        let mut eq = Self {
            bands,
            states,
            filters_l: vec![DirectForm1::<f32>::new(PASSTHROUGH); n],
            filters_r: vec![DirectForm1::<f32>::new(PASSTHROUGH); n],
            sample_rate,
            channels,
            smooth_coeff,
        };
        eq.recompute_all_coefficients();
        eq
    }

    /// Set the gain for a specific band (clamped to -24..+6 dB).
    ///
    /// The gain change is smoothed over ~10ms to avoid clicks.
    pub fn set_gain(&mut self, band_index: usize, gain_db: f32) {
        if let Some(state) = self.states.get_mut(band_index) {
            state.target_gain_db = gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        }
    }

    /// Get the band layout (frequency, Q, kind). Gains reflect target values.
    #[must_use]
    pub fn bands(&self) -> Vec<EqBandConfig> {
        self.bands
            .iter()
            .zip(self.states.iter())
            .map(|(band, state)| EqBandConfig {
                gain_db: state.target_gain_db,
                ..*band
            })
            .collect()
    }

    /// Get the target gain for a specific band.
    #[must_use]
    pub fn target_gain(&self, band_index: usize) -> Option<f32> {
        self.states.get(band_index).map(|s| s.target_gain_db)
    }

    /// Check if any band is currently smoothing (target != current).
    #[cfg(test)]
    fn is_smoothing(&self) -> bool {
        self.states
            .iter()
            .any(|s| (s.target_gain_db - s.current_gain_db).abs() > SMOOTH_CONVERGENCE_THRESHOLD)
    }

    /// Compute the one-pole smoother coefficient for the given sample rate.
    #[expect(
        clippy::cast_precision_loss,
        reason = "SMOOTH_BLOCK_SIZE is a small constant"
    )]
    fn compute_smooth_coeff(sample_rate: f32) -> f32 {
        let tau = SMOOTH_TIME_MS / MS_PER_SEC;
        // Scale by block size since we apply smoothing per-block, not per-sample
        let effective_rate = sample_rate / SMOOTH_BLOCK_SIZE as f32;
        1.0 - (-1.0 / (tau * effective_rate)).exp()
    }

    /// Recompute all coefficients from current gains (used at startup).
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    fn recompute_all_coefficients(&mut self) {
        let sr = (self.sample_rate as f32).hz();
        for (i, band) in self.bands.iter().enumerate() {
            let gain = self.states[i].current_gain_db;
            let coeffs = compute_coefficients(band, gain, sr);
            self.filters_l[i].update_coefficients(coeffs);
            self.filters_r[i].update_coefficients(coeffs);
        }
    }

    /// Advance smoothers and update coefficients for bands that changed.
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    fn smooth_and_update(&mut self) {
        let sr = (self.sample_rate as f32).hz();
        let coeff = self.smooth_coeff;

        for i in 0..self.bands.len() {
            let state = &mut self.states[i];
            let diff = state.target_gain_db - state.current_gain_db;

            if diff.abs() < SMOOTH_CONVERGENCE_THRESHOLD {
                // Close enough — snap to target
                if diff.abs() > f32::EPSILON {
                    state.current_gain_db = state.target_gain_db;
                    let coeffs = compute_coefficients(&self.bands[i], state.current_gain_db, sr);
                    self.filters_l[i].update_coefficients(coeffs);
                    self.filters_r[i].update_coefficients(coeffs);
                }
                continue;
            }

            // One-pole exponential smoothing
            state.current_gain_db += coeff * diff;
            let gain = state.current_gain_db;
            let coeffs = compute_coefficients(&self.bands[i], gain, sr);
            self.filters_l[i].update_coefficients(coeffs);
            self.filters_r[i].update_coefficients(coeffs);
        }
    }
}

impl AudioEffect for EqEffect {
    fn process(&mut self, mut chunk: PcmChunk) -> Option<PcmChunk> {
        let channels = self.channels as usize;
        if channels == 0 {
            return Some(chunk);
        }

        let samples = chunk.pcm.as_mut_slice();
        let n_bands = self.bands.len();
        let total_frames = samples.len() / channels;

        // Process in blocks for smooth coefficient updates
        let mut frame_offset = 0;
        while frame_offset < total_frames {
            // Update smoothed coefficients at block boundaries
            self.smooth_and_update();

            let block_end = (frame_offset + SMOOTH_BLOCK_SIZE).min(total_frames);
            let block_samples = &mut samples[frame_offset * channels..block_end * channels];

            for frame in block_samples.chunks_exact_mut(channels) {
                // Left channel (or mono)
                let mut sample_l = frame[0];
                for f in &mut self.filters_l[..n_bands] {
                    sample_l = f.run(sample_l);
                }
                frame[0] = sample_l;

                // Right channel (if stereo or more)
                if channels >= STEREO_CHANNELS {
                    let mut sample_r = frame[1];
                    for f in &mut self.filters_r[..n_bands] {
                        sample_r = f.run(sample_r);
                    }
                    frame[1] = sample_r;
                }
            }

            frame_offset = block_end;
        }

        Some(chunk)
    }

    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn reset(&mut self) {
        for f in &mut self.filters_l {
            *f = DirectForm1::new(PASSTHROUGH);
        }
        for f in &mut self.filters_r {
            *f = DirectForm1::new(PASSTHROUGH);
        }
        for state in &mut self.states {
            state.target_gain_db = 0.0;
            state.current_gain_db = 0.0;
        }
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
                let expected = (30.0f32 * 18000.0).sqrt();
                assert!((bands[0].frequency - expected).abs() < 1.0);
            }
            10 => {
                assert!((bands[0].frequency - 30.0).abs() < 1.0);
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
    fn eq_flat_gain_is_passthrough() {
        let bands = generate_log_spaced_bands(10);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        let num_frames = 1024;
        let mut pcm = Vec::with_capacity(num_frames * 2);
        for i in 0..num_frames {
            let sample = (2.0 * PI * 1000.0 * i as f32 / 44100.0).sin();
            pcm.push(sample);
            pcm.push(sample);
        }

        let input_copy = pcm.clone();
        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();

        let out_samples = output.samples();
        for (i, (&out, &inp)) in out_samples.iter().zip(input_copy.iter()).enumerate() {
            assert!(
                (out - inp).abs() < 1e-5,
                "Sample {i} differs: input={inp}, output={out}"
            );
        }
    }

    #[kithara::test]
    fn eq_reset_clears_gains_and_history() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        eq.set_gain(0, 6.0);
        // Process some audio to apply gain
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.5f32; 256];
        let chunk = test_chunk(spec, pcm);
        let _ = eq.process(chunk);

        eq.reset();

        // All target and current gains should be 0
        for state in &eq.states {
            assert!(
                state.target_gain_db.abs() < f32::EPSILON,
                "target should be 0 after reset"
            );
            assert!(
                state.current_gain_db.abs() < f32::EPSILON,
                "current should be 0 after reset"
            );
        }
    }

    #[kithara::test]
    fn eq_set_gain_clamps() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        eq.set_gain(0, 100.0);
        assert!((eq.target_gain(0).unwrap() - 6.0).abs() < f32::EPSILON);

        eq.set_gain(0, -100.0);
        assert!((eq.target_gain(0).unwrap() - (-24.0)).abs() < f32::EPSILON);

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

    // FilterKind tests

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
    fn eq_low_shelf_boosts_bass() {
        let bands = vec![EqBandConfig {
            frequency: 200.0,
            kind: FilterKind::LowShelf,
            gain_db: 6.0,
            ..Default::default()
        }];
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        let gain_bass = measure_sine_gain(&mut eq, 50.0, spec);
        eq.reset();
        // Re-set the gain after reset (reset zeros everything)
        eq.set_gain(0, 6.0);
        // Process enough to converge the smoother
        converge_smoother(&mut eq, spec);
        let gain_treble = measure_sine_gain(&mut eq, 4000.0, spec);

        assert!(
            gain_bass > 1.2,
            "50Hz should be boosted by LowShelf, got gain={gain_bass:.3}"
        );
        assert!(
            gain_bass > gain_treble * 1.3,
            "Bass gain ({gain_bass:.3}) should exceed treble ({gain_treble:.3})"
        );
    }

    #[kithara::test]
    fn eq_high_shelf_boosts_treble() {
        let bands = vec![EqBandConfig {
            frequency: 5000.0,
            kind: FilterKind::HighShelf,
            gain_db: 6.0,
            ..Default::default()
        }];
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        let gain_treble = measure_sine_gain(&mut eq, 10000.0, spec);
        eq.reset();
        eq.set_gain(0, 6.0);
        converge_smoother(&mut eq, spec);
        let gain_bass = measure_sine_gain(&mut eq, 200.0, spec);

        assert!(
            gain_treble > 1.5,
            "10kHz should be boosted by HighShelf, got gain={gain_treble:.3}"
        );
        assert!(
            gain_treble > gain_bass * 1.3,
            "Treble gain ({gain_treble:.3}) should exceed bass ({gain_bass:.3})"
        );
    }

    // Smooth transition tests

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
        let bands = vec![EqBandConfig {
            frequency: 1000.0,
            kind: FilterKind::Peaking,
            gain_db: 0.0,
            ..Default::default()
        }];
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        eq.set_gain(0, 6.0);

        // Process enough audio for smoother to converge (~50ms = 2205 samples)
        converge_smoother(&mut eq, spec);

        assert!(
            !eq.is_smoothing(),
            "should have converged after sufficient processing"
        );
        assert!(
            (eq.states[0].current_gain_db - 6.0).abs() < 0.01,
            "current gain should match target after convergence"
        );
    }

    #[kithara::test]
    fn eq_smooth_no_discontinuity() {
        // Verify no large sample-to-sample jumps when gain changes abruptly
        let bands = vec![EqBandConfig {
            frequency: 1000.0,
            kind: FilterKind::Peaking,
            gain_db: 0.0,
            ..Default::default()
        }];
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        // Process some audio at 0dB to establish filter state
        let warmup: Vec<f32> = (0..4096)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, warmup);
        let _ = eq.process(chunk);

        // Now change gain abruptly
        eq.set_gain(0, 6.0);

        // Process more audio and check for discontinuities
        let signal: Vec<f32> = (0..4096)
            .map(|i| (2.0 * PI * 1000.0 * (i + 4096) as f32 / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, signal);
        let output = eq.process(chunk).unwrap();
        let out = output.samples();

        // Check max sample-to-sample difference is bounded
        let max_diff = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);

        // A 1kHz sine at 44.1kHz has max derivative ≈ 2π*1000/44100 ≈ 0.143 per sample
        // With +6dB boost (2x), max should be ~0.286. Allow some headroom.
        assert!(
            max_diff < 0.5,
            "Discontinuity detected: max sample diff = {max_diff:.4}"
        );
    }

    // Helpers

    /// Process enough audio for the smoother to converge.
    fn converge_smoother(eq: &mut EqEffect, spec: PcmSpec) {
        // ~200ms of silence: 10ms time constant needs ~120 blocks (32 samples each)
        let frames = (spec.sample_rate as usize) / 5; // 200ms
        let pcm = vec![0.0f32; frames * spec.channels as usize];
        let chunk = test_chunk(spec, pcm);
        let _ = eq.process(chunk);
    }

    /// Measure the linear gain of a sine wave through the EQ.
    #[expect(
        clippy::cast_precision_loss,
        reason = "frame count and index are small integers"
    )]
    fn measure_sine_gain(eq: &mut EqEffect, freq_hz: f32, spec: PcmSpec) -> f32 {
        let num_frames = 44100; // 1 second
        let mut pcm = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            let sample = (2.0 * PI * freq_hz * i as f32 / spec.sample_rate as f32).sin();
            pcm.push(sample);
        }

        let input_rms: f32 = (pcm.iter().map(|s| s * s).sum::<f32>() / num_frames as f32).sqrt();

        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();
        let out = output.samples();

        // Skip first 4096 samples (filter transient)
        let steady = &out[4096..];
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();

        output_rms / input_rms
    }
}
