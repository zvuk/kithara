use core::{
    f32::consts::PI,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_signal::sanitize_sample;
use num_traits::ToPrimitive;

use super::{LimiterConfig, LimiterError};
use crate::consts;

pub(super) struct ScalarLimiter {
    history: [[f32; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS],
    shared_peak: [f32; Self::DETECTOR_CHANNELS],
    taps: [f32; (Self::DETECTOR_PHASES - 1) * Self::DETECTOR_TAPS],
    shared_valid: bool,
    config: LimiterConfig,
    envelope: f32,
    release_coeff: f32,
    channels: usize,
}

impl ScalarLimiter {
    const DETECTOR_CHANNELS: usize = 8;
    const DETECTOR_HALF_WIDTH: usize = 8;
    const DETECTOR_PHASES: usize = 8;
    const DETECTOR_TAPS: usize = 2 * Self::DETECTOR_HALF_WIDTH + 1;

    pub(super) fn new(
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
            config,
            release_coeff,
            taps,
            envelope: 1.0,
            channels: channels.get(),
            history: [[0.0; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS],
            shared_peak: [0.0; Self::DETECTOR_CHANNELS],
            shared_valid: false,
        })
    }

    #[inline(always)]
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

    fn detector_tap(distance: f32) -> f32 {
        if distance.abs() < f32::EPSILON {
            return 1.0;
        }
        let argument = PI * distance;
        let half_width = Self::DETECTOR_HALF_WIDTH.to_f32().unwrap_or(1.0);
        let window = 0.5 * (1.0 + (PI * distance / half_width).cos());
        argument.sin() / argument * window
    }

    #[inline(always)]
    fn inter_sample_peak(&self, block: &[f32], channel: usize, frame: usize, base: isize) -> f32 {
        let Some(history) = self.history.get(channel) else {
            return 0.0;
        };
        let mut peak = 0.0_f32;
        for branch in self.taps.as_chunks::<{ Self::DETECTOR_TAPS }>().0 {
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

    pub(super) fn process_planar(&mut self, channels: &mut [&mut [f32]]) {
        debug_assert_eq!(channels.len(), self.channels);
        debug_assert!(self.channels <= Self::DETECTOR_CHANNELS);

        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        self.shared_valid = false;
        let reconstruct = frames > 0;

        for frame in 0..frames {
            let mut peak = 0.0_f32;
            for (index, channel) in channels.iter_mut().enumerate() {
                let sample = sanitize_sample(channel[frame]);
                channel[frame] = sample;
                peak = peak.max(sample.abs());
                if reconstruct {
                    peak = peak.max(self.advance_detector(channel, index, frame));
                }
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

    fn push_history(&mut self, channel: usize, sample: f32) {
        if let Some(history) = self.history.get_mut(channel) {
            history.copy_within(1.., 0);
            if let Some(newest) = history.last_mut() {
                *newest = sample;
            }
        }
    }

    pub(super) fn reset(&mut self) {
        self.envelope = 1.0;
        self.history = [[0.0; Self::DETECTOR_HALF_WIDTH + 1]; Self::DETECTOR_CHANNELS];
        self.shared_peak = [0.0; Self::DETECTOR_CHANNELS];
        self.shared_valid = false;
    }

    #[inline]
    fn step(&mut self, peak: f32) -> f32 {
        let required = if peak > f32::from(self.config.ceiling()) {
            f32::from(self.config.ceiling()) / peak
        } else {
            1.0
        };
        self.envelope = (1.0 - self.envelope).mul_add(-self.release_coeff, 1.0);
        if required < self.envelope {
            self.envelope = required;
        }
        self.envelope
    }
}
