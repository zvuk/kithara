use core::{
    f32::consts::PI,
    num::{NonZeroU32, NonZeroUsize},
};

use arrayvec::ArrayVec;
use bon::bon;
use kithara_config::Config;
use kithara_dsp::{
    envelope::{AttackRelease, PeakHold},
    filter::Fir,
    multiply_in_place, sanitize,
};
use kithara_signal::sanitize_sample;
use num_traits::ToPrimitive;

use super::{Ceiling, ReleaseMs};
use crate::consts;

/// Channel count rejected by [`PeakLimiter::new`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LimiterError {
    #[error("limiter carries {channels} channels, more than the {limit} the detector tracks")]
    Channels { channels: usize, limit: usize },
}

/// Output ceiling and gain recovery of one [`PeakLimiter`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Config)]
#[config(builder(existing), owner_access, fields(value, get(copy)))]
#[non_exhaustive]
pub struct LimiterConfig {
    /// Linear peak the output never exceeds.
    ceiling: Ceiling,
    /// Milliseconds the gain takes to recover toward unity.
    release_ms: ReleaseMs,
}

#[bon]
impl LimiterConfig {
    #[builder(
        builder_type(vis = "pub"),
        start_fn(name = builder, vis = "pub"),
        finish_fn(vis = "pub")
    )]
    fn new(
        #[builder(default)] ceiling: Ceiling,
        #[builder(default)] release_ms: ReleaseMs,
    ) -> Self {
        Self {
            ceiling,
            release_ms,
        }
    }
}

/// Stereo-linked, zero-lookahead peak limiter: immediate attack, exponential release toward unity,
/// and a unity bypass below the ceiling that is exact for normal finite samples.
#[derive(Debug, Clone, kithara_config::ConfigOwner)]
#[config_owner(config)]
#[non_exhaustive]
pub struct PeakLimiter {
    detector: Fir<
        { Self::DETECTOR_TAPS },
        { Self::DETECTOR_PHASES - 1 },
        { Self::DETECTOR_CHANNELS },
        { Self::DETECTOR_HALF_WIDTH + 1 },
    >,
    config: LimiterConfig,
    envelope: AttackRelease,
    channels: usize,
}

impl PeakLimiter {
    const CHUNK_FRAMES: usize = 256;

    /// Channels the detector keeps history for. The limiter links its channels into one
    /// gain, so a block wider than a small surround layout has no caller here; the extra
    /// channels would silently lose their inter-sample peaks, so `new` rejects them.
    const DETECTOR_CHANNELS: usize = 8;

    /// Half the detector kernel length in input samples. Eight taps per side keep the
    /// reconstruction of a full-scale quarter-rate sinusoid at or above its true peak
    /// while staying within a real-time budget.
    const DETECTOR_HALF_WIDTH: usize = 8;

    /// Oversampling factor of the inter-sample peak detector: the reconstruction is
    /// evaluated at every eighth-sample phase, close enough that a probe never misses
    /// the crest of a partial sitting between two samples.
    const DETECTOR_PHASES: usize = 8;

    /// Taps in one polyphase branch of the detector kernel.
    const DETECTOR_TAPS: usize = 2 * Self::DETECTOR_HALF_WIDTH + 1;

    /// Build a limiter applying `config` to `channels` linked channels.
    ///
    /// # Errors
    /// Returns [`LimiterError::Channels`] when `channels` exceeds what the detector tracks.
    pub fn new(
        sample_rate: NonZeroU32,
        channels: NonZeroUsize,
        config: LimiterConfig,
    ) -> Result<Self, LimiterError> {
        let release_ms = f32::from(config.release_ms());
        if channels.get() > Self::DETECTOR_CHANNELS {
            return Err(LimiterError::Channels {
                channels: channels.get(),
                limit: Self::DETECTOR_CHANNELS,
            });
        }

        let samples = release_ms / consts::MS_PER_SEC * sample_rate.get().to_f32().unwrap_or(1.0);
        let release_coeff = (-1.0 / samples).exp();

        let taps = std::array::from_fn(|phase: usize| {
            std::array::from_fn(|tap: usize| {
                let offset = (phase + 1).to_f32().unwrap_or(0.0)
                    / Self::DETECTOR_PHASES.to_f32().unwrap_or(1.0);
                let position =
                    tap.to_f32().unwrap_or(0.0) - Self::DETECTOR_HALF_WIDTH.to_f32().unwrap_or(0.0);
                Self::detector_tap(offset - position)
            })
        });

        Ok(Self {
            config,
            detector: Fir::new(taps),
            envelope: AttackRelease::new(release_coeff),
            channels: channels.get(),
        })
    }

    /// Hann-windowed sinc evaluated `distance` input samples away from a tap.
    fn detector_tap(distance: f32) -> f32 {
        if distance.abs() < f32::EPSILON {
            return 1.0;
        }
        let argument = PI * distance;
        let half_width = Self::DETECTOR_HALF_WIDTH.to_f32().unwrap_or(1.0);
        let window = 0.5 * (1.0 + (PI * distance / half_width).cos());
        argument.sin() / argument * window
    }

    /// Apply linked gains to the common prefix without allocation.
    /// Keep sanitized current and raw future separate until pre-gain history is recorded.
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) {
        debug_assert_eq!(channels.len(), self.channels);
        debug_assert!(self.channels <= Self::DETECTOR_CHANNELS);
        let frames = channels
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0);
        let ceiling = f32::from(self.ceiling());
        let reconstruct = self.detector.begin_block(channels, ceiling);
        for start in (0..frames).step_by(Self::CHUNK_FRAMES) {
            let end = frames.min(start + Self::CHUNK_FRAMES);
            let count = end - start;
            let mut linked = ArrayVec::<f32, { Self::CHUNK_FRAMES }>::new();
            let mut current = ArrayVec::new();
            let mut peaks = ArrayVec::new();
            for plane in [&mut linked, &mut peaks] {
                plane.extend(std::iter::repeat_n(0.0, count));
            }
            let [linked, current, peaks] = [&mut linked, &mut current, &mut peaks];
            if !reconstruct {
                for (index, channel) in channels.iter_mut().enumerate() {
                    sanitize(&mut channel[start..end]);
                    let processed = self.detector.process(
                        index,
                        &channel[start..],
                        &channel[start..end],
                        peaks,
                    );
                    peaks[processed..].fill(0.0);
                }
                linked.fill(1.0);
                self.envelope.process(linked);
                if linked.iter().any(|&gain| gain != 1.0) {
                    for channel in channels.iter_mut() {
                        multiply_in_place(&mut channel[start..end], linked);
                    }
                }
                continue;
            }
            for (index, channel) in channels.iter_mut().enumerate() {
                current.clear();
                current.extend(channel[start..end].iter().copied().map(sanitize_sample));
                let processed = self
                    .detector
                    .process(index, &channel[start..], current, peaks);
                peaks[processed..].fill(0.0);
                PeakHold::link(peaks, linked);
                channel[start..end].copy_from_slice(current);
            }
            for peak in linked.iter_mut() {
                *peak = if *peak > ceiling {
                    ceiling / *peak
                } else {
                    1.0
                };
            }
            self.envelope.process(linked);
            for channel in channels.iter_mut() {
                multiply_in_place(&mut channel[start..end], linked);
            }
        }
    }

    /// Reset the gain envelope to unity and forget detector history.
    pub fn reset(&mut self) {
        self.envelope.reset();
        self.detector.reset();
    }
}

#[cfg(test)]
mod tests {
    use kithara_fixture_media::reconstruction::reconstructed_peak;
    use kithara_test_fixtures::unit_fixtures::{
        limiter_attack, limiter_half, limiter_infinity, limiter_intersample, limiter_left,
        limiter_negative, limiter_negative_infinity, limiter_peak, limiter_quiet, limiter_recovery,
        limiter_right, limiter_silence, limiter_sine, limiter_smooth, limiter_spike, limiter_two,
    };
    use kithara_test_utils::kithara;

    use super::{super::scalar, *};

    fn limiter(sample_rate: u32, release_ms: f32) -> PeakLimiter {
        PeakLimiter::new(
            NonZeroU32::new(sample_rate).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            LimiterConfig::builder()
                .ceiling(Ceiling::try_from(consts::CEILING).unwrap())
                .release_ms(ReleaseMs::try_from(release_ms).unwrap())
                .build(),
        )
        .unwrap()
    }

    fn run(limiter: &mut PeakLimiter, left: &mut [f32], right: &mut [f32]) {
        let mut chans: [&mut [f32]; 2] = [left, right];
        limiter.process_planar(&mut chans);
    }

    #[kithara::test(native, flash(false))]
    fn bounded_blocks_preserve_pcm_and_following_attack_and_release() {
        let values = [
            0.0_f32,
            -0.0,
            0.02,
            -0.03,
            0.1,
            -0.2,
            0.4,
            -0.1,
            0.98,
            0.98_f32.next_up(),
        ];
        for channel_count in [1, 2, 8] {
            for frames in [0, 1, 7, 17, 127, 255, 256, 257, 600] {
                let mut actual = PeakLimiter::new(
                    NonZeroU32::new(48_000).unwrap(),
                    NonZeroUsize::new(channel_count).unwrap(),
                    LimiterConfig::default(),
                )
                .unwrap();
                let mut reference = scalar::ScalarLimiter::new(
                    NonZeroU32::new(48_000).unwrap(),
                    NonZeroUsize::new(actual.channels).unwrap(),
                    actual.config,
                )
                .unwrap();
                for (stage, scale) in [0.25, 8.0, 0.25, 1.0, 0.01, 0.99, 2.0, 0.1]
                    .into_iter()
                    .enumerate()
                {
                    let mut channels: Vec<Vec<f32>> = (0..channel_count)
                        .map(|channel| {
                            (0..frames + channel)
                                .map(|frame| {
                                    if stage == 3 && frame == frames / 2 {
                                        [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][channel % 3]
                                    } else {
                                        values[(frame + channel) % values.len()] * scale
                                    }
                                })
                                .collect()
                        })
                        .collect();
                    let mut expected = channels.clone();
                    actual.process_planar(
                        &mut channels
                            .iter_mut()
                            .map(Vec::as_mut_slice)
                            .collect::<Vec<_>>(),
                    );
                    reference.process_planar(
                        &mut expected
                            .iter_mut()
                            .map(Vec::as_mut_slice)
                            .collect::<Vec<_>>(),
                    );
                    for (actual, expected) in
                        channels.iter().flatten().zip(expected.iter().flatten())
                    {
                        assert_eq!(actual.to_bits(), expected.to_bits());
                    }
                }
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn varying_block_sizes_preserve_history_and_untouched_tails() {
        for channels in [1, 2, 8] {
            let rate = NonZeroU32::new(48_000).unwrap();
            let channels = NonZeroUsize::new(channels).unwrap();
            let config = LimiterConfig::default();
            let mut actual = PeakLimiter::new(rate, channels, config).unwrap();
            let mut reference = scalar::ScalarLimiter::new(rate, channels, config).unwrap();
            for (stage, frames) in [600_usize, 1, 0, 7, 257, 1, 256, 0, 255, 7, 600]
                .into_iter()
                .enumerate()
            {
                let mut input: Vec<Vec<f32>> = (0..channels.get())
                    .map(|channel| {
                        (0..frames + channel)
                            .map(|frame| {
                                if stage % 4 == 3 && frame >= frames / 2 {
                                    *[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]
                                        .get(channel % 3)
                                        .unwrap()
                                } else {
                                    let sample = *[0.0, -0.0, 0.125, -0.5, 1.0, -1.0]
                                        .get((frame + channel) % 6)
                                        .unwrap();
                                    sample * if stage % 4 == 0 { 2.0 } else { 0.02 }
                                }
                            })
                            .collect()
                    })
                    .collect();
                let original = input.clone();
                let mut expected = input.clone();
                actual.process_planar(
                    &mut input.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>(),
                );
                reference.process_planar(
                    &mut expected
                        .iter_mut()
                        .map(Vec::as_mut_slice)
                        .collect::<Vec<_>>(),
                );
                for ((actual, expected), original) in input.iter().zip(&expected).zip(&original) {
                    assert_eq!(
                        actual
                            .iter()
                            .map(|sample| sample.to_bits())
                            .collect::<Vec<_>>(),
                        expected
                            .iter()
                            .map(|sample| sample.to_bits())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        actual
                            .get(frames..)
                            .unwrap()
                            .iter()
                            .map(|sample| sample.to_bits())
                            .collect::<Vec<_>>(),
                        original
                            .get(frames..)
                            .unwrap()
                            .iter()
                            .map(|sample| sample.to_bits())
                            .collect::<Vec<_>>()
                    );
                }
                if stage == 5 {
                    actual.reset();
                    reference.reset();
                }
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn an_unprocessed_channel_tail_still_controls_the_detector() {
        for tail in [2.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut actual = limiter(48_000, 50.0);
            let mut reference = scalar::ScalarLimiter::new(
                NonZeroU32::new(48_000).unwrap(),
                NonZeroUsize::new(actual.channels).unwrap(),
                actual.config,
            )
            .unwrap();
            let mut left = [0.1];
            let mut right = [0.1, tail];
            let mut expected_left = left;
            let mut expected_right = right;
            run(&mut actual, &mut left, &mut right);
            reference.process_planar(&mut [&mut expected_left, &mut expected_right]);
            for (actual, expected) in left
                .iter()
                .chain(&right)
                .zip(expected_left.iter().chain(&expected_right))
            {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn reset_restores_the_scalar_lifecycle() {
        let mut actual = limiter(48_000, 50.0);
        let mut reference = scalar::ScalarLimiter::new(
            NonZeroU32::new(48_000).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            actual.config,
        )
        .unwrap();
        for drive in [2.0, 0.1] {
            let mut input = [[drive; 257]; 2];
            let mut expected = input;
            actual.process_planar(&mut input.each_mut().map(|channel| &mut channel[..]));
            reference.process_planar(&mut expected.each_mut().map(|channel| &mut channel[..]));
            assert_eq!(input, expected);
            actual.reset();
            reference.reset();
        }
    }

    #[kithara::test(native, flash(false))]
    fn inter_sample_peaks_are_held_under_the_ceiling(limiter_intersample: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_intersample.clone();
        let mut right = limiter_intersample.clone();
        let sample_peak = left.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(
            sample_peak < consts::CEILING,
            "fixture must stay under the ceiling between samples, not at them: {sample_peak}"
        );

        run(&mut lim, &mut left, &mut right);

        let reconstructed = reconstructed_peak(&left);
        assert!(
            reconstructed <= consts::CEILING,
            "reconstructed peak {reconstructed} exceeds ceiling {}",
            consts::CEILING
        );
    }

    #[kithara::test(native, flash(false))]
    fn more_channels_than_the_detector_tracks_are_rejected() {
        let rate = NonZeroU32::new(44_100).unwrap();
        let wide = NonZeroUsize::new(9).unwrap();
        assert!(matches!(
            PeakLimiter::new(rate, wide, LimiterConfig::default()),
            Err(LimiterError::Channels { channels: 9, .. })
        ));
    }

    #[kithara::test(native, flash(false))]
    fn an_overload_is_cut_to_the_ceiling_and_no_further(limiter_sine: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_sine.clone();
        let mut right = limiter_sine.clone();

        run(&mut lim, &mut left, &mut right);

        // WHY: The detector reconstructs with a shorter kernel than this oracle, so the
        // two disagree at the kernel's own resolution. Measured here at 0.0001 dB, bounded
        // at twice that, three orders below the ceiling's own 0.175 dB of headroom.
        const RESOLUTION_DB: f32 = 0.0002;
        let reconstructed = reconstructed_peak(&left);
        let over_db = 20.0 * (reconstructed / consts::CEILING).log10();
        assert!(
            over_db <= RESOLUTION_DB,
            "reconstructed peak {reconstructed} sits {over_db} dB over ceiling {}",
            consts::CEILING
        );
        let louder: Vec<f32> = left.iter().map(|sample| sample * 1.01).collect();
        let louder = reconstructed_peak(&louder);
        assert!(
            louder > consts::CEILING,
            "one percent of headroom is left unused: {louder}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn block_length_does_not_change_what_the_detector_sees(limiter_intersample: Vec<f32>) {
        let mut whole = limiter(44_100, 50.0);
        let mut left = limiter_intersample.clone();
        let mut right = limiter_intersample.clone();
        run(&mut whole, &mut left, &mut right);

        let mut split = limiter(44_100, 50.0);
        let mut chopped_l = limiter_intersample.clone();
        let mut chopped_r = limiter_intersample.clone();
        for (block_l, block_r) in chopped_l.chunks_mut(7).zip(chopped_r.chunks_mut(7)) {
            let mut chans: [&mut [f32]; 2] = [block_l, block_r];
            split.process_planar(&mut chans);
        }

        // WHY: The last frame of a block judges the interval ahead of it against a held
        // tail, and the samples that actually follow arrive only in the next block, by
        // which time that frame has left. Without lookahead the seam therefore leaks, and
        // the leak is bounded and named rather than hidden: it must stay inside the
        // headroom the ceiling itself reserves, so the seam never reaches full scale.
        const SEAM_LEAK_DB: f32 = 0.1;
        let whole = reconstructed_peak(&left);
        let chopped = reconstructed_peak(&chopped_l);
        assert!(whole <= consts::CEILING, "a single block leaks: {whole}");
        let seam_db = 20.0 * (chopped / consts::CEILING).log10();
        assert!(
            seam_db <= SEAM_LEAK_DB,
            "seven-frame blocks leak {seam_db} dB over the ceiling"
        );
        assert!(chopped < 1.0, "the seam reaches full scale: {chopped}");
    }

    #[kithara::test(native, flash(false))]
    fn below_ceiling_is_bit_exact_unity(limiter_smooth: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let input = limiter_smooth.clone();
        let mut left = input.clone();
        let mut right = input.clone();
        run(&mut lim, &mut left, &mut right);
        assert_eq!(left, input);
        assert_eq!(right, input);
    }

    #[kithara::test(native, flash(false))]
    fn positive_peak_clamped_to_ceiling(limiter_peak: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_peak.clone();
        let mut right = limiter_peak.clone();
        run(&mut lim, &mut left, &mut right);
        for &s in &left {
            assert!(s <= consts::CEILING, "sample {s} over ceiling");
            assert!(
                s >= consts::CEILING / consts::STEP_OVERSHOOT - consts::DETECTOR_RESOLUTION,
                "sample {s} ducked deeper than the step out of silence asks"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn negative_peak_clamped_to_ceiling(limiter_negative: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_negative.clone();
        let mut right = limiter_negative.clone();
        run(&mut lim, &mut left, &mut right);
        for &s in &left {
            assert!(s >= -consts::CEILING, "sample {s} under -ceiling");
            assert!(
                s <= -consts::CEILING / consts::STEP_OVERSHOOT + consts::DETECTOR_RESOLUTION,
                "sample {s} ducked deeper than the step out of silence asks"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn no_sample_exceeds_ceiling_over_varied_input(
        limiter_right: Vec<f32>,
        limiter_left: Vec<f32>,
    ) {
        let mut lim = limiter(48_000, 50.0);
        let mut left = limiter_left.clone();
        let mut right = limiter_right.clone();
        {
            let mut chans: [&mut [f32]; 2] = [&mut left, &mut right];
            lim.process_planar(&mut chans);
        }
        for (&l, &r) in left.iter().zip(right.iter()) {
            assert!(l.abs() <= consts::CEILING + 1e-6, "left {l} over ceiling");
            assert!(r.abs() <= consts::CEILING + 1e-6, "right {r} over ceiling");
        }
    }

    #[kithara::test(native, flash(false))]
    fn channels_link_by_frame_peak(limiter_quiet: Vec<f32>, limiter_two: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_two.clone();
        let mut right = limiter_quiet.clone();
        run(&mut lim, &mut left, &mut right);
        let gain = left[0] / 2.0;
        assert!(
            (gain - consts::CEILING / consts::STEP_OVERSHOOT / 2.0).abs()
                < consts::DETECTOR_RESOLUTION,
            "the loud channel did not land a step overshoot under the ceiling: {}",
            left[0]
        );
        assert!(
            0.1f32.mul_add(-gain, right[0]).abs() < 1e-6,
            "the quiet channel took a different gain: {} vs {gain}",
            right[0] / 0.1
        );
    }

    #[kithara::test(native, flash(false))]
    fn attack_is_immediate_from_first_frame(limiter_attack: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_attack.clone();
        let mut right = limiter_attack.clone();
        run(&mut lim, &mut left, &mut right);
        assert!(
            left[0] <= consts::CEILING,
            "first frame not limited: {}",
            left[0]
        );
        assert!(
            left[0] >= consts::CEILING / consts::STEP_OVERSHOOT - consts::DETECTOR_RESOLUTION,
            "first frame ducked deeper than the step out of silence asks: {}",
            left[0]
        );
    }

    #[kithara::test(native, flash(false))]
    fn release_recovers_monotonically_toward_unity(
        limiter_half: Vec<f32>,
        limiter_spike: Vec<f32>,
    ) {
        let mut lim = limiter(44_100, 50.0);
        let mut spike_l = limiter_spike.clone();
        let mut spike_r = limiter_spike.clone();
        run(&mut lim, &mut spike_l, &mut spike_r);

        let signal = 0.5_f32;
        let mut prev_gain = 0.0_f32;
        for _ in 0..20_000 {
            let mut l = limiter_half.clone();
            let mut r = limiter_half.clone();
            run(&mut lim, &mut l, &mut r);
            let gain = l[0] / signal;
            assert!(
                gain >= prev_gain - 1e-7,
                "gain went backwards: {prev_gain} -> {gain}"
            );
            assert!(gain <= 1.0 + 1e-7);
            prev_gain = gain;
        }
        assert!(
            prev_gain > 0.99,
            "did not recover toward unity: {prev_gain}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn release_slope_is_sample_rate_derived(limiter_half: Vec<f32>, limiter_spike: Vec<f32>) {
        let drive = |lim: &mut PeakLimiter| {
            let mut sl = limiter_spike.clone();
            let mut sr = limiter_spike.clone();
            run(lim, &mut sl, &mut sr);
        };
        let recover_after = |lim: &mut PeakLimiter, frames: usize| -> f32 {
            let signal = 0.5_f32;
            let mut gain = 0.0;
            for _ in 0..frames {
                let mut l = limiter_half.clone();
                let mut r = limiter_half.clone();
                run(lim, &mut l, &mut r);
                gain = l[0] / signal;
            }
            gain
        };

        let mut slow = limiter(44_100, 50.0);
        let mut fast = limiter(96_000, 50.0);
        drive(&mut slow);
        drive(&mut fast);
        let g_low = recover_after(&mut slow, 500);
        let g_high = recover_after(&mut fast, 500);
        assert!(
            g_low > g_high,
            "expected slower per-frame recovery at 96k: {g_low} !> {g_high}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn silence_stays_silent_and_finite(limiter_silence: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_silence.clone();
        let mut right = limiter_silence.clone();
        run(&mut lim, &mut left, &mut right);
        for &s in left.iter().chain(right.iter()) {
            assert_eq!(s, 0.0);
            assert!(s.is_finite());
        }
    }

    #[kithara::test(native, flash(false))]
    fn non_finite_frame_stays_silent_without_ducking_the_next_block(
        limiter_recovery: Vec<f32>,
        limiter_negative_infinity: Vec<f32>,
        limiter_infinity: Vec<f32>,
    ) {
        let mut lim = limiter(48_000, 50.0);
        let mut spike_l = limiter_infinity.clone();
        let mut spike_r = limiter_negative_infinity.clone();
        run(&mut lim, &mut spike_l, &mut spike_r);
        assert_eq!(spike_l[0], 0.0);
        assert_eq!(spike_r[0], 0.0);

        let signal = 0.5_f32;
        let mut left = limiter_recovery.clone();
        let mut right = limiter_recovery.clone();
        run(&mut lim, &mut left, &mut right);
        assert_eq!(left, [signal; 64], "the block after lost level");
        assert_eq!(right, [signal; 64], "the block after lost level");
    }

    #[kithara::test(native, flash(false))]
    fn a_configured_ceiling_bounds_the_output(limiter_attack: Vec<f32>) {
        let ceiling = 0.5;
        let mut lim = PeakLimiter::new(
            NonZeroU32::new(44_100).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            LimiterConfig::builder()
                .ceiling(Ceiling::try_from(ceiling).unwrap())
                .build(),
        )
        .unwrap();
        let mut left = limiter_attack.clone();
        let mut right = limiter_attack;
        run(&mut lim, &mut left, &mut right);
        assert!(
            left.iter()
                .chain(&right)
                .all(|sample| sample.abs() <= ceiling),
            "every sample must stay under the configured ceiling"
        );
    }
}
