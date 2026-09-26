use std::ops::Range;

use crate::{AudioSpec, FrameCount, SignalError};

/// Checked borrowed view over frame-major interleaved samples.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct InterleavedView<'a> {
    samples: &'a [f32],
    spec: AudioSpec,
    frames: FrameCount,
}

impl<'a> InterleavedView<'a> {
    /// Validate borrowed interleaved storage against an explicit signal shape.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when channels, frame math, or storage shape is invalid.
    pub fn new(
        samples: &'a [f32],
        spec: AudioSpec,
        frames: FrameCount,
    ) -> Result<Self, SignalError> {
        let expected_samples = spec.sample_count(frames)?.get();
        if samples.len() != expected_samples {
            return Err(SignalError::Shape {
                expected_samples,
                actual_samples: samples.len(),
            });
        }
        Ok(Self {
            samples,
            spec,
            frames,
        })
    }

    /// Deinterleave into a frame offset of caller-owned channel slices.
    ///
    /// # Errors
    /// Returns [`SignalError`] when channel shapes, frame math, or destination capacity differ.
    pub fn deinterleave_channels_into_at(
        &self,
        output: &mut [&mut [f32]],
        offset: usize,
    ) -> Result<(), SignalError> {
        let channel_count = self.spec.channel_count()?;
        let channels = channel_count.get();
        if output.len() != channels {
            return Err(SignalError::ChannelCount {
                expected: channels,
                actual: output.len(),
            });
        }
        let available = output.first().map_or(0, |channel| channel.len());
        for (channel, samples) in output.iter().enumerate().skip(1) {
            if samples.len() != available {
                return Err(SignalError::ChannelFrames {
                    channel,
                    expected: available,
                    actual: samples.len(),
                });
            }
        }
        let required = offset.checked_add(self.frames.get()).ok_or_else(|| {
            SignalError::SampleCountOverflow {
                channels,
                frames: self.frames.get(),
            }
        })?;
        if available < required {
            return Err(SignalError::Capacity {
                required_samples: required.saturating_mul(channels),
                available_samples: available.saturating_mul(channels),
            });
        }
        fast_interleave::deinterleave_variable(
            self.samples,
            channel_count,
            output,
            offset..required,
        );
        Ok(())
    }

    #[must_use]
    pub const fn frames(&self) -> FrameCount {
        self.frames
    }

    /// Select a relative frame range.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `range` exceeds this view.
    pub fn range(self, range: Range<usize>) -> Result<Self, SignalError> {
        let available = self.frames.get();
        if range.start > range.end || range.end > available {
            return Err(SignalError::FrameRange {
                start: range.start,
                end: range.end,
                frames: available,
            });
        }
        let channels = self.spec.channel_count()?.get();
        let sample_start =
            range
                .start
                .checked_mul(channels)
                .ok_or(SignalError::SampleCountOverflow {
                    channels,
                    frames: range.start,
                })?;
        let sample_end =
            range
                .end
                .checked_mul(channels)
                .ok_or(SignalError::SampleCountOverflow {
                    channels,
                    frames: range.end,
                })?;
        Self::new(
            &self.samples[sample_start..sample_end],
            self.spec,
            FrameCount::new(range.end - range.start),
        )
    }

    #[must_use]
    pub const fn samples(&self) -> &'a [f32] {
        self.samples
    }

    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }
}

#[cfg(test)]
mod tests {
    use kithara_core_test_fixtures::{channel_signals, pcm_ramp, stereo_pair};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{PlanarBuffer, consts, test_pools::pools_with_budget};

    fn spec(channels: u16) -> AudioSpec {
        AudioSpec::new(channels, consts::INTERLEAVED_RATE)
    }

    #[kithara::test]
    #[case::mono(1)]
    #[case::stereo(2)]
    #[case::three_channels(3)]
    #[case::four_channels(4)]
    #[case::five_channels(5)]
    #[case::six_channels(6)]
    #[case::seven_channels(7)]
    #[case::eight_channels(8)]
    #[case::wide_nine_channels(9)]
    fn planar_round_trip(#[case] channels: u16) {
        let channel_signals = channel_signals();
        let frames = FrameCount::new(5);
        let source = &channel_signals[usize::from(channels) - 1];
        let interleaved =
            InterleavedView::new(source, spec(channels), frames).expect("fixture shape is exact");
        let pools = pools_with_budget(128 * size_of::<f32>());
        let mut planar =
            PlanarBuffer::new(&pools, spec(channels), frames).expect("fixture planar storage fits");
        let mut channel_samples = (0..usize::from(channels))
            .map(|_| vec![0.0; frames.get()])
            .collect::<Vec<_>>();
        let mut destinations = channel_samples
            .iter_mut()
            .map(Vec::as_mut_slice)
            .collect::<Vec<_>>();

        interleaved
            .deinterleave_channels_into_at(&mut destinations, 0)
            .expect("deinterleave succeeds");
        drop(destinations);
        for (channel, samples) in channel_samples.iter().enumerate() {
            planar
                .channel_mut(channel)
                .expect("planar channel exists")
                .copy_from_slice(samples);
        }
        let mut output = vec![f32::NAN; source.len()];
        let output = planar
            .view()
            .interleave_into(&mut output)
            .expect("interleave succeeds");

        assert_eq!(output.samples(), source.as_slice());
    }

    #[kithara::test]
    fn caller_channel_destination_is_checked() {
        let source = stereo_pair();
        let view = InterleavedView::new(&source, spec(2), FrameCount::new(2))
            .expect("stereo fixture shape is exact");
        let mut left = [0.0; 2];
        let mut short = [0.0; 1];

        assert_eq!(
            view.deinterleave_channels_into_at(&mut [&mut left], 0),
            Err(SignalError::ChannelCount {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(
            view.deinterleave_channels_into_at(&mut [&mut left, &mut short], 0),
            Err(SignalError::ChannelFrames {
                channel: 1,
                expected: 2,
                actual: 1,
            })
        );
    }

    #[kithara::test]
    fn caller_channel_destination_receives_each_channel() {
        let source = stereo_pair();
        let view = InterleavedView::new(&source, spec(2), FrameCount::new(2))
            .expect("stereo fixture shape is exact");
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];

        view.deinterleave_channels_into_at(&mut [&mut left, &mut right], 0)
            .expect("caller channels fit");

        assert_eq!(left, [1.0, 2.0]);
        assert_eq!(right, [3.0, 6.0]);
    }

    #[kithara::test]
    fn offset_deinterleave_preserves_surrounding_frames_and_checks_capacity() {
        let source = stereo_pair();
        let view = InterleavedView::new(&source, spec(2), FrameCount::new(2))
            .expect("stereo fixture shape");
        let mut left = [-1.0; 4];
        let mut right = [-1.0; 4];
        view.deinterleave_channels_into_at(&mut [&mut left, &mut right], 1)
            .expect("destination range fits");
        assert_eq!(left, [-1.0, 1.0, 2.0, -1.0]);
        assert_eq!(right, [-1.0, 3.0, 6.0, -1.0]);
        assert!(matches!(
            view.deinterleave_channels_into_at(&mut [&mut left, &mut right], 3),
            Err(SignalError::Capacity { .. })
        ));
        assert_eq!(left, [-1.0, 1.0, 2.0, -1.0]);
        assert_eq!(right, [-1.0, 3.0, 6.0, -1.0]);
    }

    #[kithara::test]
    fn non_zero_planar_range_interleaves_only_selected_frames() {
        let pcm_ramp = pcm_ramp();
        let pools = pools_with_budget(64 * size_of::<f32>());
        let mut planar =
            PlanarBuffer::new(&pools, spec(2), FrameCount::new(4)).expect("planar storage fits");
        planar
            .channel_mut(0)
            .expect("left channel exists")
            .copy_from_slice(&pcm_ramp[..4]);
        planar
            .channel_mut(1)
            .expect("right channel exists")
            .copy_from_slice(&pcm_ramp[4..8]);
        let mut output = [0.0; 4];

        let interleaved = planar
            .view()
            .range(1..3)
            .expect("range exists")
            .interleave_into(&mut output)
            .expect("output fits");

        assert_eq!(interleaved.samples(), [2.0, 6.0, 3.0, 7.0]);
    }

    #[kithara::test]
    fn range_shape_and_capacity_failures_are_typed() {
        let pcm_ramp = pcm_ramp();
        let source = &pcm_ramp[..4];
        assert_eq!(
            InterleavedView::new(source, spec(2), FrameCount::new(3)),
            Err(SignalError::Shape {
                expected_samples: 6,
                actual_samples: 4,
            })
        );
        let view = InterleavedView::new(source, spec(2), FrameCount::new(2))
            .expect("fixture shape is exact");
        assert_eq!(
            view.range(1..3),
            Err(SignalError::FrameRange {
                start: 1,
                end: 3,
                frames: 2,
            })
        );
    }
}
