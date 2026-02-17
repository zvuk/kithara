//! Parametric EQ effect using biquad filter cascade.
//!
//! Provides an N-band parametric equalizer implementing [`AudioEffect`].
//! Each band uses a peaking EQ biquad filter per channel, applied as a cascade.

use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type};
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

/// Configuration for a single parametric EQ band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqBandConfig {
    /// Center frequency in Hz.
    pub frequency: f32,
    /// Q factor (bandwidth control).
    pub q_factor: f32,
    /// Gain in dB (clamped to -24..+6 on set).
    pub gain_db: f32,
}

impl Default for EqBandConfig {
    fn default() -> Self {
        Self {
            frequency: 1000.0,
            q_factor: std::f32::consts::FRAC_1_SQRT_2, // Q_BUTTERWORTH_F32
            gain_db: 0.0,
        }
    }
}

/// Generate logarithmically-spaced EQ bands from 30 Hz to 18 kHz.
///
/// Q factor is scaled by `1.4 * sqrt(count / 10)` to provide narrower bands
/// when there are more of them. For a single band, the geometric mean frequency
/// of 30 Hz and 18 kHz is used.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "band count and index are small integers"
)]
pub fn generate_log_spaced_bands(count: usize) -> Vec<EqBandConfig> {
    const MIN_FREQ: f32 = 30.0;
    const MAX_FREQ: f32 = 18000.0;

    if count == 0 {
        return Vec::new();
    }

    let q_factor = 1.4 * (count as f32 / 10.0).sqrt();

    if count == 1 {
        return vec![EqBandConfig {
            frequency: (MIN_FREQ * MAX_FREQ).sqrt(),
            q_factor,
            gain_db: 0.0,
        }];
    }

    let log_min = MIN_FREQ.log10();
    let log_max = MAX_FREQ.log10();
    let log_step = (log_max - log_min) / (count - 1) as f32;

    (0..count)
        .map(|i| EqBandConfig {
            frequency: 10.0f32.powf(log_min + i as f32 * log_step),
            q_factor,
            gain_db: 0.0,
        })
        .collect()
}

/// Parametric EQ effect using biquad filter cascade.
///
/// Applies N peaking EQ biquad filters in series per channel.
/// Supports mono and stereo interleaved PCM.
pub struct EqEffect {
    bands: Vec<EqBandConfig>,
    filters_l: Vec<DirectForm1<f32>>,
    filters_r: Vec<DirectForm1<f32>>,
    sample_rate: u32,
    channels: u16,
    dirty: bool,
}

impl EqEffect {
    /// Create a new EQ effect with the given bands and audio format.
    #[must_use]
    pub fn new(bands: Vec<EqBandConfig>, sample_rate: u32, channels: u16) -> Self {
        let n = bands.len();
        let mut eq = Self {
            bands,
            filters_l: vec![DirectForm1::<f32>::new(PASSTHROUGH); n],
            filters_r: vec![DirectForm1::<f32>::new(PASSTHROUGH); n],
            sample_rate,
            channels,
            dirty: true,
        };
        eq.update_coefficients();
        eq
    }

    /// Set the gain for a specific band (clamped to -24..+6 dB).
    pub fn set_gain(&mut self, band_index: usize, gain_db: f32) {
        if let Some(band) = self.bands.get_mut(band_index) {
            band.gain_db = gain_db.clamp(-24.0, 6.0);
            self.dirty = true;
        }
    }

    /// Get the current band configurations.
    #[must_use]
    pub fn bands(&self) -> &[EqBandConfig] {
        &self.bands
    }

    /// Whether coefficients need recomputation.
    #[cfg(test)]
    fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Recompute biquad coefficients from current band config.
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rate fits in f32 for audio"
    )]
    fn update_coefficients(&mut self) {
        for (i, band) in self.bands.iter().enumerate() {
            let coeffs = if band.gain_db.abs() < 0.01 {
                // Near-zero gain: passthrough (avoid unnecessary computation)
                PASSTHROUGH
            } else {
                Coefficients::<f32>::from_params(
                    Type::PeakingEQ(band.gain_db),
                    (self.sample_rate as f32).hz(),
                    band.frequency.hz(),
                    band.q_factor,
                )
                .unwrap_or(PASSTHROUGH)
            };
            self.filters_l[i] = DirectForm1::new(coeffs);
            self.filters_r[i] = DirectForm1::new(coeffs);
        }
        self.dirty = false;
    }
}

impl AudioEffect for EqEffect {
    fn process(&mut self, mut chunk: PcmChunk) -> Option<PcmChunk> {
        if self.dirty {
            self.update_coefficients();
        }

        let channels = self.channels as usize;
        if channels == 0 {
            return Some(chunk);
        }

        let samples = chunk.pcm.as_mut_slice();
        let n_bands = self.bands.len();

        for frame in samples.chunks_exact_mut(channels) {
            // Left channel (or mono)
            let mut sample_l = frame[0];
            for f in &mut self.filters_l[..n_bands] {
                sample_l = f.run(sample_l);
            }
            frame[0] = sample_l;

            // Right channel (if stereo or more)
            if channels >= 2 {
                let mut sample_r = frame[1];
                for f in &mut self.filters_r[..n_bands] {
                    sample_r = f.run(sample_r);
                }
                frame[1] = sample_r;
            }
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
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::pcm_pool;
    use kithara_decode::{PcmMeta, PcmSpec};

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

    #[test]
    fn generates_10_log_spaced_bands() {
        let bands = generate_log_spaced_bands(10);
        assert_eq!(bands.len(), 10);

        // First band near 30 Hz
        assert!((bands[0].frequency - 30.0).abs() < 1.0);
        // Last band near 18 kHz
        assert!((bands[9].frequency - 18000.0).abs() < 1.0);

        // Ascending frequency order
        for pair in bands.windows(2) {
            assert!(pair[1].frequency > pair[0].frequency);
        }

        // All gains at 0
        for band in &bands {
            assert!((band.gain_db).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn generates_1_band_centered() {
        let bands = generate_log_spaced_bands(1);
        assert_eq!(bands.len(), 1);
        // Geometric mean of 30 and 18000
        let expected = (30.0f32 * 18000.0).sqrt();
        assert!((bands[0].frequency - expected).abs() < 1.0);
    }

    #[test]
    fn generates_0_bands_empty() {
        let bands = generate_log_spaced_bands(0);
        assert!(bands.is_empty());
    }

    #[test]
    fn eq_flat_gain_is_passthrough() {
        let bands = generate_log_spaced_bands(10);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        // Generate a simple test signal: 1kHz sine, stereo
        let num_frames = 1024;
        let mut pcm = Vec::with_capacity(num_frames * 2);
        for i in 0..num_frames {
            let sample = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44100.0).sin();
            pcm.push(sample); // L
            pcm.push(sample); // R
        }

        let input_copy = pcm.clone();
        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();

        // All bands at 0dB gain should produce output very close to input
        let out_samples = output.samples();
        for (i, (&out, &inp)) in out_samples.iter().zip(input_copy.iter()).enumerate() {
            assert!(
                (out - inp).abs() < 1e-5,
                "Sample {i} differs: input={inp}, output={out}"
            );
        }
    }

    #[test]
    fn eq_reset_marks_dirty() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        // After construction, dirty should be false (coefficients computed)
        assert!(!eq.is_dirty());

        eq.reset();
        assert!(eq.is_dirty());
    }

    #[test]
    fn eq_set_gain_clamps() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        eq.set_gain(0, 100.0);
        assert!((eq.bands()[0].gain_db - 6.0).abs() < f32::EPSILON);

        eq.set_gain(0, -100.0);
        assert!((eq.bands()[0].gain_db - (-24.0)).abs() < f32::EPSILON);

        eq.set_gain(0, 3.0);
        assert!((eq.bands()[0].gain_db - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn eq_set_gain_out_of_bounds_band_is_noop() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);

        // Should not panic
        eq.set_gain(99, 5.0);
        // Bands unchanged
        for band in eq.bands() {
            assert!((band.gain_db).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn eq_process_returns_some() {
        let bands = generate_log_spaced_bands(5);
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        // Set non-zero gain to exercise filter path
        eq.set_gain(2, 3.0);

        let pcm = vec![0.5f32; 256];
        let chunk = test_chunk(spec, pcm);
        let result = eq.process(chunk);
        assert!(result.is_some());
    }

    #[test]
    fn eq_flush_returns_none() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = EqEffect::new(bands, 44100, 2);
        assert!(eq.flush().is_none());
    }

    #[test]
    fn eq_mono_processing() {
        let bands = generate_log_spaced_bands(3);
        let spec = PcmSpec {
            channels: 1,
            sample_rate: 44100,
        };
        let mut eq = EqEffect::new(bands, spec.sample_rate, spec.channels);

        let pcm = vec![0.5f32; 128];
        let chunk = test_chunk(spec, pcm);
        let result = eq.process(chunk);
        assert!(result.is_some());
        assert_eq!(result.unwrap().samples().len(), 128);
    }
}
