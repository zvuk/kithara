use kithara_bufpool::{HasPool, PoolError};
use kithara_signal::AudioChunk;

use super::{EqBandConfig, EqConfig, GainDb, IsolatorEq};
use crate::effects::AudioEffect;

#[non_exhaustive]
pub struct EqEffect {
    eq_l: IsolatorEq,
    eq_r: IsolatorEq,
    bands: Vec<EqBandConfig>,
    channels: u16,
}

impl EqEffect {
    /// Create a new EQ effect with the given bands and audio format.
    pub fn new<S>(
        config: &EqConfig<S>,
        bands: Vec<EqBandConfig>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let eq_l = IsolatorEq::new(config, &bands, sample_rate)?;
        let eq_r = IsolatorEq::new(config, &bands, sample_rate)?;
        Ok(Self {
            eq_l,
            eq_r,
            bands,
            channels,
        })
    }

    /// Get the band layout. Gains reflect target values.
    #[must_use]
    pub fn bands(&self) -> Vec<EqBandConfig> {
        self.bands
            .iter()
            .enumerate()
            .map(|(index, band)| {
                let mut band = *band;
                band.set_gain_db(self.eq_l.target_gain(index).unwrap_or_default());
                band
            })
            .collect()
    }

    /// Set the gain for a specific band.
    pub fn set_gain(&mut self, band_index: usize, gain_db: GainDb) {
        self.eq_l.set_gain(band_index, gain_db);
        self.eq_r.set_gain(band_index, gain_db);
    }

    delegate::delegate! {
        to self.eq_l {
            /// Check if any band is currently smoothing.
            #[cfg(test)]
            fn is_smoothing(&self) -> bool;
            /// Get the target gain for a specific band.
            #[must_use]
            pub fn target_gain(&self, band_index: usize) -> Option<GainDb>;
        }
    }
}

impl AudioEffect for EqEffect {
    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let channels = self.channels as usize;
        if channels == 0 {
            return Some(chunk);
        }

        let samples = &mut chunk.samples;

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

    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_utils::kithara;

    use super::{super::*, *};
    use crate::test_pools::{Pools, pools, sample_buffer};

    struct EqFixture;

    impl EqFixture {
        fn spec(channels: u16, hz: u32) -> AudioSpec {
            AudioSpec::new(channels, NonZeroU32::new(hz).expect("test rate"))
        }
    }

    fn test_chunk(pools: &Pools, spec: AudioSpec, samples: &[f32]) -> AudioChunk {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                ..Default::default()
            },
            sample_buffer(pools, samples),
        )
    }

    fn make_eq(
        pools: &Pools,
        bands: Vec<EqBandConfig>,
        sample_rate: u32,
        channels: u16,
    ) -> EqEffect {
        let config = EqConfig::builder(pools.clone()).build();
        EqEffect::new(&config, bands, sample_rate, channels)
            .unwrap_or_else(|error| panic!("test EQ: {error}"))
    }

    fn make_isolator(pools: &Pools, bands: &[EqBandConfig], sample_rate: u32) -> IsolatorEq {
        let config = EqConfig::builder(pools.clone()).build();
        IsolatorEq::new(&config, bands, sample_rate)
            .unwrap_or_else(|error| panic!("test isolator: {error}"))
    }

    #[kithara::test]
    fn eq_flat_gain_preserves_magnitude() {
        let pools = pools();
        let bands = generate_log_spaced_bands(10);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        let warmup = vec![0.0f32; 4096];
        let _ = eq.process(test_chunk(&pools, spec, &warmup));

        let num_frames: u16 = 44100;
        let samples: Vec<f32> = (0..num_frames)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i) / 44100.0).sin())
            .collect();

        let input_rms: f32 =
            (samples.iter().map(|s| s * s).sum::<f32>() / f32::from(num_frames)).sqrt();

        let chunk = test_chunk(&pools, spec, &samples);
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
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_eq(&pools, bands, 44100, 2);

        eq.set_gain(0, GainDb::from(100.0));
        assert_eq!(eq.target_gain(0).unwrap(), GainDb::MAX);

        eq.set_gain(0, GainDb::from(-100.0));
        assert_eq!(eq.target_gain(0).unwrap(), GainDb::MIN);

        eq.set_gain(0, GainDb::from(3.0));
        assert_eq!(eq.target_gain(0).unwrap(), GainDb::from(3.0));
    }

    #[kithara::test]
    fn eq_set_gain_out_of_bounds_band_is_noop() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_eq(&pools, bands, 44100, 2);
        eq.set_gain(99, GainDb::from(5.0));
        for i in 0..3 {
            assert_eq!(eq.target_gain(i).unwrap(), GainDb::DEFAULT);
        }
    }

    #[kithara::test]
    fn eq_reset_clears_gains_and_history() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_eq(&pools, bands, 44100, 2);

        eq.set_gain(0, GainDb::MAX);
        let spec = EqFixture::spec(2, 44100);
        let samples = vec![0.5f32; 256];
        let chunk = test_chunk(&pools, spec, &samples);
        let _ = eq.process(chunk);

        eq.reset();

        for i in 0..3 {
            assert_eq!(
                eq.target_gain(i).unwrap(),
                GainDb::DEFAULT,
                "target should be unity after reset"
            );
        }
    }

    #[kithara::test]
    fn eq_single_band_kill() {
        let pools = pools();
        let bands = vec![EqBandConfig::builder().frequency(1000.0).build()];
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, GainDb::MIN);
        converge_smoother(&pools, &mut eq, spec);

        let gain = measure_sine_gain(&pools, &mut eq, 1000.0, spec);
        assert!(
            gain < 0.001,
            "single band at min should be killed, got gain={gain:.6}"
        );
    }

    #[kithara::test]
    fn eq_3band_kill_low() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, GainDb::MIN);
        converge_smoother(&pools, &mut eq, spec);

        let gain_bass = measure_sine_gain(&pools, &mut eq, 40.0, spec);
        let gain_treble = measure_sine_gain(&pools, &mut eq, 10000.0, spec);
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
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(2, GainDb::MIN);
        converge_smoother(&pools, &mut eq, spec);

        let gain_treble = measure_sine_gain(&pools, &mut eq, 15000.0, spec);
        let gain_bass = measure_sine_gain(&pools, &mut eq, 40.0, spec);
        assert!(
            gain_treble < 0.05,
            "treble should be killed, got {gain_treble:.4}"
        );
        assert!(gain_bass > 0.8, "bass should pass, got {gain_bass:.4}");
    }

    #[kithara::test]
    fn eq_3band_kill_all_produces_silence() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        for i in 0..3 {
            eq.set_gain(i, GainDb::MIN);
        }
        converge_smoother(&pools, &mut eq, spec);

        for freq in [40.0, 1000.0, 10000.0] {
            let gain = measure_sine_gain(&pools, &mut eq, freq, spec);
            assert!(
                gain < 0.001,
                "all bands killed: {freq}Hz gain should be ~0, got {gain:.6}"
            );
        }
    }

    #[kithara::test]
    fn eq_low_shelf_boosts_bass() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, GainDb::MAX);
        converge_smoother(&pools, &mut eq, spec);

        let gain_bass = measure_sine_gain(&pools, &mut eq, 40.0, spec);
        assert!(
            gain_bass > 1.5,
            "40Hz should be boosted, got gain={gain_bass:.3}"
        );
    }

    #[kithara::test]
    fn eq_high_shelf_boosts_treble() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(2, GainDb::MAX);
        converge_smoother(&pools, &mut eq, spec);

        let gain_treble = measure_sine_gain(&pools, &mut eq, 15000.0, spec);
        assert!(
            gain_treble > 1.5,
            "15kHz should be boosted, got gain={gain_treble:.3}"
        );
    }

    #[kithara::test]
    fn eq_gain_change_starts_smoothing() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_eq(&pools, bands, 44100, 2);

        assert!(!eq.is_smoothing(), "should not be smoothing initially");
        eq.set_gain(0, GainDb::MAX);
        assert!(eq.is_smoothing(), "should be smoothing after set_gain");
    }

    #[kithara::test]
    fn eq_smooth_gain_converges() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, GainDb::MAX);

        converge_smoother(&pools, &mut eq, spec);

        assert!(
            !eq.is_smoothing(),
            "should have converged after sufficient processing"
        );
    }

    #[kithara::test]
    fn eq_smooth_no_discontinuity() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        let warmup: Vec<f32> = (0u16..4096)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i) / 44100.0).sin())
            .collect();
        let chunk = test_chunk(&pools, spec, &warmup);
        let _ = eq.process(chunk);

        eq.set_gain(0, GainDb::MAX);

        let signal: Vec<f32> = (0u16..4096)
            .map(|i| (2.0 * PI * 1000.0 * f32::from(i + 4096) / 44100.0).sin())
            .collect();
        let chunk = test_chunk(&pools, spec, &signal);
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
        let pools = pools();
        let bands = generate_log_spaced_bands(5);
        let spec = EqFixture::spec(channels, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        if let Some((band, gain_db)) = gain {
            eq.set_gain(band, GainDb::from(gain_db));
        }

        let samples = vec![0.5f32; sample_len];
        let chunk = test_chunk(&pools, spec, &samples);
        let result = eq.process(chunk);
        assert!(result.is_some());
        assert_eq!(result.unwrap().samples.len(), sample_len);
    }

    #[kithara::test]
    fn eq_flush_returns_none() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_eq(&pools, bands, 44100, 2);
        assert!(eq.flush().is_none());
    }

    #[kithara::test]
    fn eq_output_never_nan_or_inf() {
        let pools = pools();
        let bands = generate_log_spaced_bands(10);
        let spec = EqFixture::spec(2, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        for round in 0..100 {
            let gain = if round % 2 == 0 {
                GainDb::MAX
            } else {
                GainDb::MIN
            };
            for band in 0..10 {
                eq.set_gain(band, gain);
            }

            let samples: Vec<f32> = (0u16..1024).map(|i| (f32::from(i) * 0.1).sin()).collect();
            let chunk = test_chunk(&pools, spec, &samples);
            let output = eq.process(chunk).unwrap();
            for (i, &s) in output.samples.iter().enumerate() {
                assert!(s.is_finite(), "round {round} sample {i}: got {s}");
            }
        }
    }

    #[kithara::test]
    fn eq_nan_input_produces_safe_output() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);
        eq.set_gain(0, GainDb::MAX);
        converge_smoother(&pools, &mut eq, spec);

        let mut samples = vec![0.5f32; 256];
        samples[10] = f32::NAN;
        samples[20] = f32::INFINITY;
        samples[30] = f32::NEG_INFINITY;
        let chunk = test_chunk(&pools, spec, &samples);
        let output = eq.process(chunk).unwrap();

        for (i, &s) in output.samples.iter().enumerate() {
            assert!(s.is_finite(), "sample {i}: got {s}");
        }
    }

    #[kithara::test]
    fn eq_extreme_gain_oscillation_stays_safe() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(2, 44100);
        let mut eq = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        for round in 0..200 {
            let gain = if round % 2 == 0 {
                GainDb::MAX
            } else {
                GainDb::MIN
            };
            // The range is asymmetric, so the opposite of the floor is not
            // the ceiling: it clamps back down to it.
            let opposite = GainDb::from(-f32::from(gain));
            eq.set_gain(0, gain);
            eq.set_gain(1, opposite);
            eq.set_gain(2, gain);

            let samples: Vec<f32> = (0u16..512).map(|i| (f32::from(i) * 0.3).sin()).collect();
            let chunk = test_chunk(&pools, spec, &samples);
            let output = eq.process(chunk).unwrap();
            for &s in &output.samples[..] {
                assert!(s.is_finite());
            }
        }
    }

    #[kithara::test]
    fn eq_fresh_at_zero_db_is_bypass_active() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let eq = make_isolator(&pools, &bands, 44100);
        assert!(
            eq.bypass_active(),
            "default 0 dB bands should activate bypass so the LR-4 chain \
             never runs for users who never touch the EQ"
        );
    }

    #[kithara::test]
    fn eq_bypass_deactivates_on_gain_change() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_isolator(&pools, &bands, 44100);
        assert!(eq.bypass_active(), "precondition: fresh EQ is in bypass");

        eq.set_gain(0, GainDb::from(3.0));

        assert!(
            !eq.bypass_active(),
            "bypass must deactivate the instant any band targets a non-unity \
             gain, so the next sample reaches the actual filter chain"
        );
    }

    #[kithara::test]
    fn eq_bypass_reactivates_after_return_to_unity() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq_effect = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        eq_effect.set_gain(0, GainDb::MAX);
        converge_smoother(&pools, &mut eq_effect, spec);
        assert!(!eq_effect.eq_l.bypass_active());

        eq_effect.set_gain(0, GainDb::default());
        converge_smoother(&pools, &mut eq_effect, spec);
        converge_smoother(&pools, &mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.bypass_active(),
            "after gains smooth back to unity, bypass must reactivate so the \
             filter chain stops running"
        );
    }

    #[kithara::test]
    fn eq_bypass_returns_input_unchanged() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_isolator(&pools, &bands, 44100);
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
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let spec = EqFixture::spec(1, 44100);
        let mut eq_effect = make_eq(&pools, bands, spec.sample_rate.get(), spec.channels);

        for i in 0..3 {
            eq_effect.set_gain(i, GainDb::MIN);
        }
        converge_smoother(&pools, &mut eq_effect, spec);

        assert!(
            eq_effect.eq_l.silence_active(),
            "all bands at the floor of the range after the smoother converges must \
             the silence fast path so the filter chain is skipped entirely"
        );
    }

    #[kithara::test]
    fn eq_silence_returns_zero() {
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_isolator(&pools, &bands, 44100);
        for i in 0..3 {
            eq.set_gain(i, GainDb::MIN);
            eq.settle_gain(i);
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
        let pools = pools();
        let bands = generate_log_spaced_bands(3);
        let mut eq = make_isolator(&pools, &bands, 44100);
        for i in 0..3 {
            eq.set_gain(i, GainDb::MIN);
            eq.settle_gain(i);
        }
        assert!(eq.silence_active(), "precondition: silence is active");

        eq.set_gain(1, GainDb::from(-3.0));

        assert!(
            !eq.silence_active(),
            "raising any band above the floor must disable silence so the \
             filter chain re-engages via smoother ramp-up"
        );
    }

    fn converge_smoother(pools: &Pools, eq: &mut EqEffect, spec: AudioSpec) {
        let frames = (spec.sample_rate.get() as usize) / 5;
        let samples = vec![0.0f32; frames * spec.channels as usize];
        let chunk = test_chunk(pools, spec, &samples);
        let _ = eq.process(chunk);
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "frame count and index are small integers"
    )]
    fn measure_sine_gain(pools: &Pools, eq: &mut EqEffect, freq_hz: f32, spec: AudioSpec) -> f32 {
        let num_frames = 44100;
        let mut samples = Vec::with_capacity(num_frames);
        for i in 0..num_frames {
            let sample = (2.0 * PI * freq_hz * i as f32 / spec.sample_rate.get() as f32).sin();
            samples.push(sample);
        }

        let input_rms: f32 =
            (samples.iter().map(|s| s * s).sum::<f32>() / num_frames as f32).sqrt();

        let chunk = test_chunk(pools, spec, &samples);
        let output = eq.process(chunk).unwrap();
        let out = &output.samples[..];

        let steady = &out[4096..];
        let output_rms: f32 =
            (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();

        output_rms / input_rms
    }
}
