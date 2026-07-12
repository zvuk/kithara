use kithara_decode::PcmChunk;

use super::{EqBandConfig, IsolatorEq};
use crate::AudioEffect;

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

    /// Get the band layout. Gains reflect target values.
    #[must_use]
    pub fn bands(&self) -> Vec<EqBandConfig> {
        self.bands
            .iter()
            .enumerate()
            .map(|(index, band)| {
                let mut band = *band;
                band.set_gain_db(self.eq_l.target_gain(index).unwrap_or(0.0));
                band
            })
            .collect()
    }

    /// Check if any band is currently smoothing.
    #[cfg(test)]
    fn is_smoothing(&self) -> bool {
        self.eq_l.is_smoothing()
    }

    /// Set the gain for a specific band (clamped to min/max dB).
    pub fn set_gain(&mut self, band_index: usize, gain_db: f32) {
        self.eq_l.set_gain(band_index, gain_db);
        self.eq_r.set_gain(band_index, gain_db);
    }

    /// Get the target gain for a specific band.
    #[must_use]
    pub fn target_gain(&self, band_index: usize) -> Option<f32> {
        self.eq_l.target_gain(band_index)
    }
}

impl AudioEffect for EqEffect {
    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn process(&mut self, mut chunk: PcmChunk) -> Option<PcmChunk> {
        let channels = self.channels as usize;
        if channels == 0 {
            return Some(chunk);
        }

        let samples = chunk.samples.as_mut_slice();

        for frame in samples.chunks_exact_mut(channels) {
            frame[0] = self.eq_l.process_sample(frame[0]);
            if channels >= 2 {
                frame[1] = self.eq_r.process_sample(frame[1]);
            }
        }

        Some(chunk)
    }

    fn reset(&mut self) {
        self.eq_l.reset();
        self.eq_r.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::{f32::consts::PI, num::NonZeroU32};

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;

    use super::{super::*, *};

    struct EqFixture;

    impl EqFixture {
        fn spec(channels: u16, hz: u32) -> PcmSpec {
            PcmSpec::new(channels, NonZeroU32::new(hz).expect("test rate"))
        }
    }

    fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            PcmPool::default().attach(pcm),
        )
    }

    #[kithara::test]
    fn eq_flat_gain_preserves_magnitude() {
        let bands = generate_log_spaced_bands(10);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        let warmup = vec![0.0f32; 4096];
        let _ = eq.process(test_chunk(spec, warmup));

        let num_frames: u16 = 44100;
        let pcm: Vec<f32> = (0..num_frames)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i) / 44100.0).sin())
            .collect();

        let input_rms: f32 =
            (pcm.iter().map(|s| s * s).sum::<f32>() / f32::from(num_frames)).sqrt();

        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();
        let out = &output.samples[..];

        let steady = &out[4096..];
        let steady_len = u16::try_from(steady.len()).expect("test fixture steady < u16::MAX");
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / f32::from(steady_len)).sqrt();
        let gain = output_rms / input_rms;

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
        let spec = EqFixture::spec(2, 44100);
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
        let bands = vec![EqBandConfig::builder().frequency(1000.0).build()];
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
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
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        let warmup: Vec<f32> = (0u16..4096)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i) / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, warmup);
        let _ = eq.process(chunk);

        eq.set_gain(0, MAX_GAIN_DB);

        let signal: Vec<f32> = (0u16..4096)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i + 4096) / 44100.0).sin())
            .collect();
        let chunk = test_chunk(spec, signal);
        let output = eq.process(chunk).unwrap();
        let out = &output.samples[..];

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
        let spec = EqFixture::spec(channels, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
        if let Some((band, gain_db)) = gain {
            eq.set_gain(band, gain_db);
        }

        let pcm = vec![0.5f32; sample_len];
        let chunk = test_chunk(spec, pcm);
        let result = eq.process(chunk);
        assert!(result.is_some());
        assert_eq!(result.unwrap().samples.len(), sample_len);
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
        let spec = EqFixture::spec(2, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        for round in 0..100 {
            let gain = if round % 2 == 0 {
                MAX_GAIN_DB
            } else {
                MIN_GAIN_DB
            };
            for band in 0..10 {
                eq.set_gain(band, gain);
            }

            let pcm: Vec<f32> = (0u16..1024).map(|i| (f32::from(i) * 0.1).sin()).collect();
            let chunk = test_chunk(spec, pcm);
            let output = eq.process(chunk).unwrap();
            for (i, &s) in output.samples.iter().enumerate() {
                assert!(s.is_finite(), "round {round} sample {i}: got {s}");
            }
        }
    }

    #[kithara::test]
    fn eq_nan_input_produces_safe_output() {
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, 6.0);
        converge_smoother(&mut eq, spec);

        let mut pcm = vec![0.5f32; 256];
        pcm[10] = f32::NAN;
        pcm[20] = f32::INFINITY;
        pcm[30] = f32::NEG_INFINITY;
        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();

        for (i, &s) in output.samples.iter().enumerate() {
            assert!(s.is_finite(), "sample {i}: got {s}");
        }
    }

    #[kithara::test]
    fn eq_extreme_gain_oscillation_stays_safe() {
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(2, 44100);
        let mut eq = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        for round in 0..200 {
            let gain = if round % 2 == 0 {
                MAX_GAIN_DB
            } else {
                MIN_GAIN_DB
            };
            eq.set_gain(0, gain);
            eq.set_gain(1, -gain);
            eq.set_gain(2, gain);

            let pcm: Vec<f32> = (0u16..512).map(|i| (f32::from(i) * 0.3).sin()).collect();
            let chunk = test_chunk(spec, pcm);
            let output = eq.process(chunk).unwrap();
            for &s in &output.samples[..] {
                assert!(s.is_finite());
            }
        }
    }

    #[kithara::test]
    fn eq_fresh_at_zero_db_is_bypass_active() {
        let bands = generate_log_spaced_bands(3);
        let eq = IsolatorEq::new(&bands, 44100);
        assert!(
            eq.bypass_active(),
            "default 0 dB bands should activate bypass so the LR-4 chain \
             never runs for users who never touch the EQ"
        );
    }

    #[kithara::test]
    fn eq_bypass_deactivates_on_gain_change() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        assert!(eq.bypass_active(), "precondition: fresh EQ is in bypass");

        eq.set_gain(0, 3.0);

        assert!(
            !eq.bypass_active(),
            "bypass must deactivate the instant any band targets a non-unity \
             gain, so the next sample reaches the actual filter chain"
        );
    }

    #[kithara::test]
    fn eq_bypass_reactivates_after_return_to_unity() {
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq_effect = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        eq_effect.set_gain(0, 6.0);
        converge_smoother(&mut eq_effect, spec);
        assert!(!eq_effect.eq_l.bypass_active());

        eq_effect.set_gain(0, 0.0);
        converge_smoother(&mut eq_effect, spec);
        converge_smoother(&mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.bypass_active(),
            "after gains smooth back to unity, bypass must reactivate so the \
             filter chain stops running"
        );
    }

    #[kithara::test]
    fn eq_bypass_returns_input_unchanged() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        assert!(eq.bypass_active(), "precondition: bypass is active");

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
        let spec = EqFixture::spec(1, 44100);
        let mut eq_effect = EqEffect::new(bands, spec.sample_rate.get(), spec.channels);

        for i in 0..3 {
            eq_effect.set_gain(i, MIN_GAIN_DB);
        }
        converge_smoother(&mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.silence_active(),
            "all bands at MIN_GAIN_DB after smoother converges must activate \
             the silence fast path so the filter chain is skipped entirely"
        );
    }

    #[kithara::test]
    fn eq_silence_returns_zero() {
        let bands = generate_log_spaced_bands(3);
        let mut eq = IsolatorEq::new(&bands, 44100);
        for i in 0..3 {
            eq.set_gain(i, MIN_GAIN_DB);
            eq.force_current_gain(i, 0.0);
        }
        assert!(eq.silence_active(), "precondition: silence is active");

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
            eq.set_gain(i, MIN_GAIN_DB);
            eq.force_current_gain(i, 0.0);
        }
        assert!(eq.silence_active(), "precondition: silence is active");

        eq.set_gain(1, -3.0);

        assert!(
            !eq.silence_active(),
            "raising any band above MIN_GAIN_DB must disable silence so the \
             filter chain re-engages via smoother ramp-up"
        );
    }

    fn converge_smoother(eq: &mut EqEffect, spec: PcmSpec) {
        let frames = (spec.sample_rate.get() as usize) / 5;
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
            let sample = (2.0 * PI * freq_hz * i as f32 / spec.sample_rate.get() as f32).sin();
            pcm.push(sample);
        }

        let input_rms: f32 = (pcm.iter().map(|s| s * s).sum::<f32>() / num_frames as f32).sqrt();

        let chunk = test_chunk(spec, pcm);
        let output = eq.process(chunk).unwrap();
        let out = &output.samples[..];

        let steady = &out[4096..];
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();

        output_rms / input_rms
    }
}
