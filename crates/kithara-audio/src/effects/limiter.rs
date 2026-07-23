use core::num::{NonZeroU32, NonZeroUsize};

use num_traits::ToPrimitive;

/// Configuration rejected by [`PeakLimiter::new`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LimiterError {
    #[error("limiter ceiling {ceiling} is not finite in (0.0, 1.0]")]
    Ceiling { ceiling: f32 },

    #[error("limiter release {release_ms} ms is not finite and positive")]
    Release { release_ms: f32 },
}

/// Stereo-linked, zero-lookahead peak limiter: immediate attack, exponential
/// release toward unity, exact unity bypass below the ceiling.
#[derive(Debug, Clone)]
pub struct PeakLimiter {
    envelope: f32,
    ceiling: f32,
    release_coeff: f32,
    channels: usize,
}

impl PeakLimiter {
    /// Build a limiter with a linear `ceiling` and `release_ms` recovery.
    ///
    /// # Errors
    /// Returns [`LimiterError`] when `ceiling` is outside `(0.0, 1.0]` or
    /// `release_ms` is not finite and positive.
    pub fn new(
        sample_rate: NonZeroU32,
        channels: NonZeroUsize,
        ceiling: f32,
        release_ms: f32,
    ) -> Result<Self, LimiterError> {
        if !ceiling.is_finite() || ceiling <= 0.0 || ceiling > 1.0 {
            return Err(LimiterError::Ceiling { ceiling });
        }
        if !release_ms.is_finite() || release_ms <= 0.0 {
            return Err(LimiterError::Release { release_ms });
        }

        let samples = release_ms / 1000.0 * sample_rate.get().to_f32().unwrap_or(1.0);
        let release_coeff = (-1.0 / samples).exp();

        Ok(Self {
            envelope: 1.0,
            ceiling,
            release_coeff,
            channels: channels.get(),
        })
    }

    /// Reset the gain envelope to unity.
    pub fn reset(&mut self) {
        self.envelope = 1.0;
    }

    /// Apply the limiter in place to a planar block, linking channels by frame
    /// peak. Allocates nothing, locks nothing, performs no I/O.
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) {
        debug_assert_eq!(channels.len(), self.channels);

        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        for frame in 0..frames {
            let mut peak = 0.0_f32;
            for channel in channels.iter() {
                peak = peak.max(channel[frame].abs());
            }
            let gain = self.step(peak);
            for channel in channels.iter_mut() {
                channel[frame] *= gain;
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
        // Release before the clamp: the reverse order lets the recovered gain
        // overshoot the ceiling for one frame.
        self.envelope = 1.0 - (1.0 - self.envelope) * self.release_coeff;
        if required < self.envelope {
            self.envelope = required;
        }
        self.envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CEILING: f32 = 0.98;

    fn limiter(sample_rate: u32, release_ms: f32) -> PeakLimiter {
        PeakLimiter::new(
            NonZeroU32::new(sample_rate).unwrap(),
            NonZeroUsize::new(2).unwrap(),
            CEILING,
            release_ms,
        )
        .unwrap()
    }

    fn run(limiter: &mut PeakLimiter, left: &mut [f32], right: &mut [f32]) {
        let mut chans: [&mut [f32]; 2] = [left, right];
        limiter.process_planar(&mut chans);
    }

    #[test]
    fn below_ceiling_is_bit_exact_unity() {
        let mut lim = limiter(44_100, 50.0);
        let input = [0.5, -0.3, 0.1, -0.7, 0.0, 0.97, -0.97];
        let mut left = input;
        let mut right = input;
        run(&mut lim, &mut left, &mut right);
        assert_eq!(left, input);
        assert_eq!(right, input);
    }

    #[test]
    fn positive_peak_clamped_to_ceiling() {
        let mut lim = limiter(44_100, 50.0);
        let mut left = [2.0_f32; 8];
        let mut right = [2.0_f32; 8];
        run(&mut lim, &mut left, &mut right);
        for &s in &left {
            assert!(
                (s - CEILING).abs() < 1e-6,
                "sample {s} not clamped to ceiling"
            );
        }
    }

    #[test]
    fn negative_peak_clamped_to_ceiling() {
        let mut lim = limiter(44_100, 50.0);
        let mut left = [-2.0_f32; 8];
        let mut right = [-2.0_f32; 8];
        run(&mut lim, &mut left, &mut right);
        for &s in &left {
            assert!(
                (s + CEILING).abs() < 1e-6,
                "sample {s} not clamped to -ceiling"
            );
        }
    }

    #[test]
    fn no_sample_exceeds_ceiling_over_varied_input() {
        let mut lim = limiter(48_000, 50.0);
        let mut left: Vec<f32> = (0..512).map(|i| (i as f32 * 0.13).sin() * 3.0).collect();
        let mut right: Vec<f32> = (0..512).map(|i| (i as f32 * 0.07).cos() * 2.5).collect();
        {
            let mut chans: [&mut [f32]; 2] = [&mut left, &mut right];
            lim.process_planar(&mut chans);
        }
        for (&l, &r) in left.iter().zip(right.iter()) {
            assert!(l.abs() <= CEILING + 1e-6, "left {l} over ceiling");
            assert!(r.abs() <= CEILING + 1e-6, "right {r} over ceiling");
        }
    }

    #[test]
    fn channels_link_by_frame_peak() {
        let mut lim = limiter(44_100, 50.0);
        let mut left = [2.0_f32];
        let mut right = [0.1_f32];
        run(&mut lim, &mut left, &mut right);
        let gain = CEILING / 2.0;
        assert!((left[0] - 2.0 * gain).abs() < 1e-6);
        assert!((right[0] - 0.1 * gain).abs() < 1e-6);
    }

    #[test]
    fn attack_is_immediate_from_first_frame() {
        let mut lim = limiter(44_100, 50.0);
        let mut left = [2.0_f32; 4];
        let mut right = [2.0_f32; 4];
        run(&mut lim, &mut left, &mut right);
        assert!((left[0] - CEILING).abs() < 1e-6, "first frame not limited");
    }

    #[test]
    fn release_recovers_monotonically_toward_unity() {
        let mut lim = limiter(44_100, 50.0);
        let mut spike_l = [4.0_f32];
        let mut spike_r = [4.0_f32];
        run(&mut lim, &mut spike_l, &mut spike_r);

        let signal = 0.5_f32;
        let mut prev_gain = 0.0_f32;
        for _ in 0..20_000 {
            let mut l = [signal];
            let mut r = [signal];
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

    #[test]
    fn release_slope_is_sample_rate_derived() {
        let drive = |lim: &mut PeakLimiter| {
            let mut sl = [4.0_f32];
            let mut sr = [4.0_f32];
            run(lim, &mut sl, &mut sr);
        };
        let recover_after = |lim: &mut PeakLimiter, frames: usize| -> f32 {
            let signal = 0.5_f32;
            let mut gain = 0.0;
            for _ in 0..frames {
                let mut l = [signal];
                let mut r = [signal];
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

    #[test]
    fn silence_stays_silent_and_finite() {
        let mut lim = limiter(44_100, 50.0);
        let mut left = [0.0_f32; 16];
        let mut right = [0.0_f32; 16];
        run(&mut lim, &mut left, &mut right);
        for &s in left.iter().chain(right.iter()) {
            assert_eq!(s, 0.0);
            assert!(s.is_finite());
        }
    }

    #[test]
    fn invalid_config_is_rejected() {
        let sr = NonZeroU32::new(44_100).unwrap();
        let ch = NonZeroUsize::new(2).unwrap();
        assert!(PeakLimiter::new(sr, ch, 0.0, 50.0).is_err());
        assert!(PeakLimiter::new(sr, ch, 1.5, 50.0).is_err());
        assert!(PeakLimiter::new(sr, ch, f32::NAN, 50.0).is_err());
        assert!(PeakLimiter::new(sr, ch, CEILING, 0.0).is_err());
        assert!(PeakLimiter::new(sr, ch, CEILING, -5.0).is_err());
        assert!(PeakLimiter::new(sr, ch, CEILING, f32::INFINITY).is_err());
    }
}
