use core::num::{NonZeroU32, NonZeroUsize};

use kithara_signal::sanitize_sample;
use num_traits::ToPrimitive;

struct Consts;

impl Consts {
    /// Milliseconds per second: the release time arrives in ms, the coefficient
    /// is computed in samples.
    const MS_PER_SEC: f32 = 1000.0;
    const DEFAULT_CEILING: f32 = 0.98;
    const DEFAULT_RELEASE_MS: f32 = 50.0;
}

/// Configuration rejected by [`LimiterConfig`] or [`PeakLimiter::new`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LimiterError {
    #[error("limiter ceiling {ceiling} is not finite in (0.0, 1.0]")]
    Ceiling { ceiling: f32 },

    #[error("limiter release {release_ms} ms is not finite and positive")]
    Release { release_ms: f32 },

    #[error("limiter carries {channels} channels, more than the {limit} the detector tracks")]
    Channels { channels: usize, limit: usize },
}

/// Output ceiling and gain recovery of one [`PeakLimiter`].
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, copy)]
#[non_exhaustive]
pub struct LimiterConfig {
    /// Linear peak the output never exceeds, in `(0.0, 1.0]`.
    ceiling: f32,
    /// Milliseconds the gain takes to recover toward unity.
    release_ms: f32,
}

#[bon::bon]
impl LimiterConfig {
    #[builder(
        builder_type(vis = "pub"),
        start_fn(name = builder, vis = "pub"),
        finish_fn(vis = "pub")
    )]
    fn new(
        #[builder(default = Consts::DEFAULT_CEILING)] ceiling: f32,
        #[builder(default = Consts::DEFAULT_RELEASE_MS)] release_ms: f32,
    ) -> Result<Self, LimiterError> {
        if !ceiling.is_finite() || ceiling <= 0.0 || ceiling > 1.0 {
            return Err(LimiterError::Ceiling { ceiling });
        }
        if !release_ms.is_finite() || release_ms <= 0.0 {
            return Err(LimiterError::Release { release_ms });
        }
        Ok(Self {
            ceiling,
            release_ms,
        })
    }
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            ceiling: Consts::DEFAULT_CEILING,
            release_ms: Consts::DEFAULT_RELEASE_MS,
        }
    }
}

/// Stereo-linked, zero-lookahead peak limiter: immediate attack, exponential release toward unity,
/// and a unity bypass below the ceiling that is exact for normal finite samples.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PeakLimiter {
    ceiling: f32,
    envelope: f32,
    release_coeff: f32,
    channels: usize,
    /// Polyphase windowed-sinc taps for the inter-sample phases, phase-major.
    taps: [f32; (Self::DETECTOR_PHASES - 1) * Self::DETECTOR_TAPS],
    /// The last `DETECTOR_HALF_WIDTH` input samples of every channel, oldest first, so a
    /// detector window reaching before the block still reads the signal as it entered the
    /// limiter rather than as it left.
    history: [[f32; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS],
    /// The peak the previous frame already reconstructed for the interval it shares with
    /// this one. Both frames read the same untouched input there, so the sum is identical
    /// and computing it twice buys nothing.
    shared_peak: [f32; Self::DETECTOR_CHANNELS],
    /// Whether [`Self::shared_peak`] describes the interval behind the next frame. It does
    /// not at the start of a block, where that interval was last judged against a held
    /// tail rather than the samples that actually followed.
    shared_valid: bool,
}

impl PeakLimiter {
    /// Oversampling factor of the inter-sample peak detector: the reconstruction is
    /// evaluated at every eighth-sample phase, close enough that a probe never misses
    /// the crest of a partial sitting between two samples.
    const DETECTOR_PHASES: usize = 8;

    /// Half the detector kernel length in input samples. Eight taps per side keep the
    /// reconstruction of a full-scale quarter-rate sinusoid at or above its true peak
    /// while staying within a real-time budget.
    const DETECTOR_HALF_WIDTH: usize = 8;

    /// Taps in one polyphase branch of the detector kernel.
    const DETECTOR_TAPS: usize = 2 * Self::DETECTOR_HALF_WIDTH + 1;

    /// Channels the detector keeps history for. The limiter links its channels into one
    /// gain, so a block wider than a small surround layout has no caller here; the extra
    /// channels would silently lose their inter-sample peaks, so `new` rejects them.
    const DETECTOR_CHANNELS: usize = 8;

    /// Hann-windowed sinc evaluated `distance` input samples away from a tap.
    fn detector_tap(distance: f32) -> f32 {
        if distance.abs() < f32::EPSILON {
            return 1.0;
        }
        let argument = core::f32::consts::PI * distance;
        let half_width = Self::DETECTOR_HALF_WIDTH.to_f32().unwrap_or(1.0);
        let window = 0.5 * (1.0 + (core::f32::consts::PI * distance / half_width).cos());
        argument.sin() / argument * window
    }

    /// Build a limiter applying `config` to `channels` linked channels.
    ///
    /// # Errors
    /// Returns [`LimiterError::Channels`] when `channels` exceeds what the detector tracks.
    pub fn new(
        sample_rate: NonZeroU32,
        channels: NonZeroUsize,
        config: LimiterConfig,
    ) -> Result<Self, LimiterError> {
        let ceiling = config.ceiling();
        let release_ms = config.release_ms();
        if channels.get() > Self::DETECTOR_CHANNELS {
            return Err(LimiterError::Channels {
                channels: channels.get(),
                limit: Self::DETECTOR_CHANNELS,
            });
        }

        let samples = release_ms / Consts::MS_PER_SEC * sample_rate.get().to_f32().unwrap_or(1.0);
        let release_coeff = (-1.0 / samples).exp();

        let mut taps = [0.0_f32; (Self::DETECTOR_PHASES - 1) * Self::DETECTOR_TAPS];
        for (index, coefficient) in taps.iter_mut().enumerate() {
            let phase = index / Self::DETECTOR_TAPS + 1;
            let tap = index % Self::DETECTOR_TAPS;
            let offset =
                phase.to_f32().unwrap_or(0.0) / Self::DETECTOR_PHASES.to_f32().unwrap_or(1.0);
            let position =
                tap.to_f32().unwrap_or(0.0) - Self::DETECTOR_HALF_WIDTH.to_f32().unwrap_or(0.0);
            *coefficient = Self::detector_tap(offset - position);
        }

        Ok(Self {
            ceiling,
            release_coeff,
            envelope: 1.0,
            channels: channels.get(),
            taps,
            history: [[0.0; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS],
            shared_peak: [0.0; Self::DETECTOR_CHANNELS],
            shared_valid: false,
        })
    }

    /// Apply the limiter in place to a planar block, linking channels by frame peak. Each sample is
    /// guarded before the peak is taken, so the envelope only ever sees a finite peak. Allocates
    /// nothing, locks nothing, performs no I/O.
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) {
        debug_assert_eq!(channels.len(), self.channels);
        debug_assert!(self.channels <= Self::DETECTOR_CHANNELS);

        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        // WHY: The interval behind frame 0 was last judged against a held tail. The samples
        // that actually followed are in this block, so it is judged again rather than taken
        // from the cache.
        self.shared_valid = false;

        for frame in 0..frames {
            let mut peak = 0.0_f32;
            for (index, channel) in channels.iter_mut().enumerate() {
                let sample = sanitize_sample(channel[frame]);
                channel[frame] = sample;
                peak = peak.max(sample.abs());
                peak = peak.max(self.advance_detector(channel, index, frame));
            }
            for (index, channel) in channels.iter().enumerate() {
                self.push_history(index, channel[frame]);
            }

            self.shared_valid = true;

            let gain = self.step(peak);
            for channel in channels.iter_mut() {
                channel[frame] *= gain;
            }
        }
    }

    /// Reset the gain envelope to unity and forget the detector history.
    pub fn reset(&mut self) {
        self.envelope = 1.0;
        self.history = [[0.0; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS];
        self.shared_peak = [0.0; Self::DETECTOR_CHANNELS];
        self.shared_valid = false;
    }

    /// Peak of both reconstructed intervals touching `frame`. The one behind it is bounded
    /// by this gain as much as by the previous frame's, and only the smaller of the two
    /// keeps its crest under the ceiling. Within a block the previous frame already
    /// reconstructed that interval from the same untouched input, so it is reused.
    fn advance_detector(&mut self, block: &[f32], channel: usize, frame: usize) -> f32 {
        let ahead = self.inter_sample_peak(block, channel, frame, frame as isize);
        let behind = if self.shared_valid {
            self.shared_peak.get(channel).copied().unwrap_or(0.0)
        } else {
            self.inter_sample_peak(block, channel, frame, frame as isize - 1)
        };
        if let Some(shared) = self.shared_peak.get_mut(channel) {
            *shared = ahead;
        }
        ahead.max(behind)
    }

    /// Largest magnitude the reconstruction reaches between `base` and the sample after it
    /// on one channel; `base` indexes `block` and may name the frame before `frame`.
    /// Everything before `frame` comes from the input history, because the block already
    /// carries the gain applied to those frames and feeding that back would make the
    /// detector chase itself; samples past the block end hold the last one, because the
    /// stream continues into the next block and reading silence there would modulate the
    /// gain at every block boundary.
    fn inter_sample_peak(&self, block: &[f32], channel: usize, frame: usize, base: isize) -> f32 {
        let Some(history) = self.history.get(channel) else {
            return 0.0;
        };
        let mut peak = 0.0_f32;
        for branch in self.taps.chunks_exact(Self::DETECTOR_TAPS) {
            let mut sum = 0.0_f32;
            for (tap, coefficient) in branch.iter().enumerate() {
                let position = base + tap as isize - Self::DETECTOR_HALF_WIDTH as isize;
                let sample = if position < frame as isize {
                    let age = frame as isize - position;
                    usize::try_from(Self::DETECTOR_HALF_WIDTH as isize + 1 - age)
                        .ok()
                        .and_then(|slot| history.get(slot).copied())
                        .unwrap_or(0.0)
                } else {
                    usize::try_from(position)
                        .ok()
                        .and_then(|index| block.get(index).or_else(|| block.last()).copied())
                        .unwrap_or(0.0)
                };
                sum = coefficient.mul_add(sample, sum);
            }
            peak = peak.max(sum.abs());
        }
        peak
    }

    /// Record one input sample as the newest detector history for its channel.
    fn push_history(&mut self, channel: usize, sample: f32) {
        if let Some(history) = self.history.get_mut(channel) {
            history.copy_within(1.., 0);
            if let Some(newest) = history.last_mut() {
                *newest = sample;
            }
        }
    }

    #[inline]
    fn step(&mut self, peak: f32) -> f32 {
        let required = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        // WHY: Release before the clamp: the reverse order lets the recovered gain overshoot the ceiling for one frame.
        self.envelope = (1.0 - self.envelope).mul_add(-self.release_coeff, 1.0);
        if required < self.envelope {
            self.envelope = required;
        }
        self.envelope
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_fixtures::unit_fixtures::{
        limiter_attack, limiter_half, limiter_infinity, limiter_intersample, limiter_left,
        limiter_negative, limiter_negative_infinity, limiter_peak, limiter_quiet, limiter_recovery,
        limiter_right, limiter_silence, limiter_sine, limiter_smooth, limiter_spike, limiter_two,
    };
    use kithara_test_utils::kithara;

    use super::*;

    /// Levels the tests measure against.
    struct Level;

    impl Level {
        const CEILING: f32 = 0.98;

        /// How far a reconstructed level may sit from the ceiling. The detector's kernel is
        /// shorter than a full reconstruction, and on the fixtures here the two disagree by
        /// about two parts in ten thousand.
        const DETECTOR_RESOLUTION: f32 = 1e-3;

        /// How far above its samples a step out of silence reconstructs. A block that starts at
        /// full level carries that overshoot, and the limiter has to duck by it; the fixtures
        /// below start exactly that way, so their output lands a step overshoot under the
        /// ceiling rather than on it.
        const STEP_OVERSHOOT: f32 = 1.1351;
    }

    fn limiter(sample_rate: u32, release_ms: f32) -> PeakLimiter {
        PeakLimiter::new(
            NonZeroU32::new(sample_rate).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            LimiterConfig::builder()
                .ceiling(Level::CEILING)
                .release_ms(release_ms)
                .build()
                .unwrap(),
        )
        .unwrap()
    }

    fn run(limiter: &mut PeakLimiter, left: &mut [f32], right: &mut [f32]) {
        let mut chans: [&mut [f32]; 2] = [left, right];
        limiter.process_planar(&mut chans);
    }

    /// Reconstruct the continuous waveform by windowed-sinc interpolation and
    /// return its largest magnitude. Independent of the limiter's own detector.
    fn reconstructed_peak(samples: &[f32]) -> f32 {
        const PHASES: usize = 16;
        const HALF_WIDTH: isize = 32;
        let mut peak = 0.0_f32;
        // WHY: The trailing window is cut because the signal continues past the buffer in
        // the stream the limiter serves, so a decay to silence there is the test's artefact
        // and not the limiter's output. The leading edge is real and stays measured.
        let tail = HALF_WIDTH as usize;
        for index in 0..samples.len().saturating_sub(tail) {
            for phase in 0..PHASES {
                let offset = phase as f32 / PHASES as f32;
                let mut value = 0.0_f32;
                for tap in -HALF_WIDTH..=HALF_WIDTH {
                    let position = index as isize + tap;
                    let Ok(position) = usize::try_from(position) else {
                        continue;
                    };
                    let Some(&sample) = samples.get(position) else {
                        continue;
                    };
                    let distance = offset - tap as f32;
                    let sinc = if distance.abs() < 1e-6 {
                        1.0
                    } else {
                        let argument = core::f32::consts::PI * distance;
                        argument.sin() / argument
                    };
                    let window = 0.5
                        * (1.0 + (core::f32::consts::PI * distance / HALF_WIDTH as f32).cos())
                            .max(0.0);
                    value += sample * sinc * window;
                }
                peak = peak.max(value.abs());
            }
        }
        peak
    }

    #[kithara::test(native, flash(false))]
    fn inter_sample_peaks_are_held_under_the_ceiling(limiter_intersample: Vec<f32>) {
        let mut lim = limiter(44_100, 50.0);
        let mut left = limiter_intersample.clone();
        let mut right = limiter_intersample.clone();
        let sample_peak = left.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(
            sample_peak < Level::CEILING,
            "fixture must stay under the ceiling between samples, not at them: {sample_peak}"
        );

        run(&mut lim, &mut left, &mut right);

        let reconstructed = reconstructed_peak(&left);
        assert!(
            reconstructed <= Level::CEILING,
            "reconstructed peak {reconstructed} exceeds ceiling {}",
            Level::CEILING
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
        let over_db = 20.0 * (reconstructed / Level::CEILING).log10();
        assert!(
            over_db <= RESOLUTION_DB,
            "reconstructed peak {reconstructed} sits {over_db} dB over ceiling {}",
            Level::CEILING
        );
        let louder: Vec<f32> = left.iter().map(|sample| sample * 1.01).collect();
        let louder = reconstructed_peak(&louder);
        assert!(
            louder > Level::CEILING,
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
        assert!(whole <= Level::CEILING, "a single block leaks: {whole}");
        let seam_db = 20.0 * (chopped / Level::CEILING).log10();
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
            assert!(s <= Level::CEILING, "sample {s} over ceiling");
            assert!(
                s >= Level::CEILING / Level::STEP_OVERSHOOT - Level::DETECTOR_RESOLUTION,
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
            assert!(s >= -Level::CEILING, "sample {s} under -ceiling");
            assert!(
                s <= -Level::CEILING / Level::STEP_OVERSHOOT + Level::DETECTOR_RESOLUTION,
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
            assert!(l.abs() <= Level::CEILING + 1e-6, "left {l} over ceiling");
            assert!(r.abs() <= Level::CEILING + 1e-6, "right {r} over ceiling");
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
            (gain - Level::CEILING / Level::STEP_OVERSHOOT / 2.0).abs()
                < Level::DETECTOR_RESOLUTION,
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
            left[0] <= Level::CEILING,
            "first frame not limited: {}",
            left[0]
        );
        assert!(
            left[0] >= Level::CEILING / Level::STEP_OVERSHOOT - Level::DETECTOR_RESOLUTION,
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
    fn invalid_config_is_rejected() {
        let config = |ceiling: f32, release_ms: f32| {
            LimiterConfig::builder()
                .ceiling(ceiling)
                .release_ms(release_ms)
                .build()
        };
        assert!(config(0.0, 50.0).is_err());
        assert!(config(1.5, 50.0).is_err());
        assert!(config(f32::NAN, 50.0).is_err());
        assert!(config(Level::CEILING, 0.0).is_err());
        assert!(config(Level::CEILING, -5.0).is_err());
        assert!(config(Level::CEILING, f32::INFINITY).is_err());
    }

    #[kithara::test(native, flash(false))]
    fn a_configured_ceiling_bounds_the_output(limiter_attack: Vec<f32>) {
        let ceiling = 0.5;
        let mut lim = PeakLimiter::new(
            NonZeroU32::new(44_100).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            LimiterConfig::builder().ceiling(ceiling).build().unwrap(),
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
