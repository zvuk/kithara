use std::ops::Range;

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};

use crate::{AudioSpec, FrameCount, InterleavedView, SignalError, consts};

/// Pool-backed channel-major samples with independent logical length and stride.
#[derive(Debug)]
#[non_exhaustive]
pub struct PlanarBuffer {
    spec: AudioSpec,
    frames: FrameCount,
    stride: FrameCount,
    samples: SampleBuffer,
}

impl PlanarBuffer {
    /// Acquire one channel-major buffer from the caller's pool region.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the shape is invalid or pool capacity is exhausted.
    pub fn new<S>(
        pools: &PoolRegion<S>,
        spec: AudioSpec,
        frames: FrameCount,
    ) -> Result<Self, SignalError>
    where
        S: HasPool<f32>,
    {
        spec.channel_count()?;
        let mut buffer = Self {
            spec,
            frames: FrameCount::default(),
            samples: pools.get::<f32>(),
            stride: FrameCount::default(),
        };
        buffer.resize_frames(frames)?;
        Ok(buffer)
    }

    /// Complete channel-major storage, including reserved stride.
    #[must_use]
    pub fn as_samples(&self) -> &[f32] {
        &self.samples
    }

    /// Complete channel-major storage, including reserved stride.
    #[must_use]
    pub fn as_samples_mut(&mut self) -> &mut [f32] {
        &mut self.samples
    }

    /// Borrow one logical channel.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `channel` is out of range.
    pub fn channel(&self, channel: usize) -> Result<&[f32], SignalError> {
        self.view().channel(channel)
    }

    /// Mutably borrow one logical channel.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `channel` is out of range.
    pub fn channel_mut(&mut self, channel: usize) -> Result<&mut [f32], SignalError> {
        let range = channel_range(
            channel,
            self.spec.channel_count()?.get(),
            self.stride.get(),
            0,
            self.frames.get(),
        )?;
        Ok(&mut self.samples[range])
    }

    /// Reset the logical queue while retaining pooled storage and stride.
    pub const fn clear(&mut self) {
        self.frames = FrameCount::new(0);
    }

    #[must_use]
    pub const fn frames(&self) -> FrameCount {
        self.frames
    }

    fn reserve_frames(&mut self, frames: FrameCount) -> Result<(), SignalError> {
        if frames <= self.stride {
            return Ok(());
        }

        let channels = self.spec.channel_count()?.get();
        let requested_frames = frames.get();
        let requested_samples = self.spec.sample_count(frames)?.get();
        self.samples
            .ensure_len(requested_samples)
            .map_err(|_| SignalError::PoolCapacity {
                required_samples: requested_samples,
            })?;

        let amortized_stride = self
            .stride
            .get()
            .checked_mul(2)
            .unwrap_or(requested_frames)
            .max(requested_frames);
        let available_stride = self.samples.capacity() / channels;
        let stride = FrameCount::new(amortized_stride.min(available_stride));
        let required = self.spec.sample_count(stride)?.get();
        self.samples
            .ensure_len(required)
            .map_err(|_| SignalError::PoolCapacity {
                required_samples: required,
            })?;

        let old_stride = self.stride.get();
        let new_stride = stride.get();
        let logical_frames = self.frames.get();
        for channel in (0..channels).rev() {
            let old_start = channel * old_stride;
            let new_start = channel * new_stride;
            self.samples
                .copy_within(old_start..old_start + logical_frames, new_start);
            self.samples[new_start + logical_frames..new_start + new_stride].fill(0.0);
        }
        self.stride = stride;
        Ok(())
    }

    /// Change the logical frame count, growing pooled storage when necessary.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the requested storage cannot be represented or reserved.
    pub fn resize_frames(&mut self, frames: FrameCount) -> Result<(), SignalError> {
        self.reserve_frames(frames)?;
        if frames > self.frames {
            let channels = self.spec.channel_count()?.get();
            let old_frames = self.frames.get();
            for channel in 0..channels {
                let start = channel * self.stride.get() + old_frames;
                let end = channel * self.stride.get() + frames.get();
                self.samples[start..end].fill(0.0);
            }
        }
        self.frames = frames;
        Ok(())
    }

    /// Compact channel stride and release pooled capacity beyond the logical frames.
    /// This may reallocate and must run outside real-time rendering.
    pub fn shrink_to_fit(&mut self) {
        let frames = self.frames.get();
        let channels = usize::from(self.spec.channels);
        if self.stride != self.frames {
            for channel in 1..channels {
                let start = channel * self.stride.get();
                self.samples
                    .copy_within(start..start + frames, channel * frames);
            }
            self.stride = self.frames;
        }
        self.samples.truncate(channels * frames);
        self.samples.shrink_to_fit();
    }

    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }

    /// Per-channel storage stride, including reserved frames beyond the logical end.
    #[must_use]
    pub const fn stride(&self) -> FrameCount {
        self.stride
    }

    /// Remove frames from the front of every channel without reallocating.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `frames` exceeds the logical frame count.
    pub fn truncate_front(&mut self, frames: FrameCount) -> Result<(), SignalError> {
        let remove = frames.get();
        let logical = self.frames.get();
        if remove > logical {
            return Err(SignalError::FrameRange {
                start: 0,
                end: remove,
                frames: logical,
            });
        }

        let retained = logical - remove;
        let channels = self.spec.channel_count()?.get();
        for channel in 0..channels {
            let start = channel * self.stride.get();
            self.samples
                .copy_within(start + remove..start + logical, start);
            self.samples[start + retained..start + logical].fill(0.0);
        }
        self.frames = FrameCount::new(retained);
        Ok(())
    }

    #[must_use]
    pub fn view(&self) -> PlanarView<'_> {
        PlanarView {
            frames: self.frames,
            samples: &self.samples,
            spec: self.spec,
            start: 0,
            stride: self.stride,
        }
    }
}

/// Borrowed channel-major samples.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PlanarView<'a> {
    samples: &'a [f32],
    spec: AudioSpec,
    frames: FrameCount,
    stride: FrameCount,
    start: usize,
}

impl<'a> PlanarView<'a> {
    /// Borrow one channel from this view.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `channel` is out of range.
    pub fn channel(&self, channel: usize) -> Result<&'a [f32], SignalError> {
        let range = channel_range(
            channel,
            self.spec.channel_count()?.get(),
            self.stride.get(),
            self.start,
            self.frames.get(),
        )?;
        Ok(&self.samples[range])
    }

    #[must_use]
    pub const fn frames(&self) -> FrameCount {
        self.frames
    }

    /// Interleave this view into caller-owned storage.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the shape is invalid or output is too small.
    pub fn interleave_into<'out>(
        &self,
        output: &'out mut [f32],
    ) -> Result<InterleavedView<'out>, SignalError> {
        let required = self.spec.sample_count(self.frames)?.get();
        if output.len() < required {
            return Err(SignalError::Capacity {
                required_samples: required,
                available_samples: output.len(),
            });
        }
        let output = &mut output[..required];
        let channel_count = self.spec.channel_count()?;
        let channels = channel_count.get();
        if channels <= consts::FAST_CHANNELS {
            let mut input = [&[][..]; consts::FAST_CHANNELS];
            for (channel, slot) in input.iter_mut().enumerate().take(channels) {
                *slot = self.channel(channel)?;
            }
            fast_interleave::interleave_variable(
                &input[..channels],
                0..self.frames.get(),
                output,
                channel_count,
            );
        } else {
            for frame in 0..self.frames.get() {
                for channel in 0..channels {
                    output[frame * channels + channel] = self.channel(channel)?[frame];
                }
            }
        }
        InterleavedView::new(output, self.spec, self.frames)
    }

    /// Select a relative frame range without allocating channel metadata.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when `range` exceeds this view.
    pub fn range(self, range: Range<usize>) -> Result<Self, SignalError> {
        let (start, frames) = subrange(self.start, self.frames.get(), range)?;
        Ok(Self {
            frames,
            start,
            samples: self.samples,
            spec: self.spec,
            stride: self.stride,
        })
    }

    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }

    #[must_use]
    pub const fn stride(&self) -> FrameCount {
        self.stride
    }
}

fn channel_range(
    channel: usize,
    channels: usize,
    stride: usize,
    start: usize,
    frames: usize,
) -> Result<Range<usize>, SignalError> {
    if channel >= channels {
        return Err(SignalError::ChannelRange { channel, channels });
    }
    let begin = channel
        .checked_mul(stride)
        .and_then(|base| base.checked_add(start))
        .ok_or(SignalError::SampleCountOverflow {
            channels,
            frames: stride,
        })?;
    let end = begin
        .checked_add(frames)
        .ok_or(SignalError::SampleCountOverflow { frames, channels })?;
    Ok(begin..end)
}

fn subrange(
    base: usize,
    available: usize,
    range: Range<usize>,
) -> Result<(usize, FrameCount), SignalError> {
    if range.start > range.end || range.end > available {
        return Err(SignalError::FrameRange {
            start: range.start,
            end: range.end,
            frames: available,
        });
    }
    let start = base
        .checked_add(range.start)
        .ok_or(SignalError::FrameRange {
            start: range.start,
            end: range.end,
            frames: available,
        })?;
    Ok((start, FrameCount::new(range.end - range.start)))
}

#[cfg(test)]
mod tests {

    use kithara_core_test_fixtures::{negative_pcm_ramp, pcm_ramp};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools_with_budget;

    fn stereo() -> AudioSpec {
        AudioSpec::new(2, consts::INTERLEAVED_RATE)
    }

    #[kithara::test]
    fn reserve_resize_and_front_truncation_preserve_channel_major_data() {
        let pcm_ramp = pcm_ramp();
        let pools = pools_with_budget(128 * size_of::<f32>());
        let mut planar = PlanarBuffer::new(&pools, stereo(), FrameCount::new(3))
            .expect("initial planar storage fits");
        planar
            .channel_mut(0)
            .expect("left channel exists")
            .copy_from_slice(&pcm_ramp[..3]);
        planar
            .channel_mut(1)
            .expect("right channel exists")
            .copy_from_slice(&pcm_ramp[3..6]);

        planar
            .reserve_frames(FrameCount::new(8))
            .expect("stride growth fits");
        assert_eq!(planar.stride(), FrameCount::new(8));
        assert_eq!(
            planar.channel(0).expect("left channel exists"),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(
            planar.channel(1).expect("right channel exists"),
            [4.0, 5.0, 6.0]
        );

        planar
            .resize_frames(FrameCount::new(5))
            .expect("logical growth stays reserved");
        assert_eq!(
            planar.channel(0).expect("left channel exists"),
            [1.0, 2.0, 3.0, 0.0, 0.0]
        );
        planar
            .truncate_front(FrameCount::new(2))
            .expect("front range exists");
        assert_eq!(
            planar.channel(0).expect("left channel exists"),
            [3.0, 0.0, 0.0]
        );
        assert_eq!(
            planar.channel(1).expect("right channel exists"),
            [6.0, 0.0, 0.0]
        );
    }

    #[kithara::test]
    fn incremental_resize_grows_stride_geometrically() {
        let pcm_ramp = pcm_ramp();
        let negative_pcm_ramp = negative_pcm_ramp();
        let pools = pools_with_budget(1_024 * size_of::<f32>());
        let mut planar = PlanarBuffer::new(&pools, stereo(), FrameCount::new(0))
            .expect("empty planar storage is valid");
        let mut growths = 0;
        let mut previous_stride = planar.stride();

        for frames in 1..=129 {
            planar
                .resize_frames(FrameCount::new(frames))
                .expect("incremental growth fits");
            if planar.stride() != previous_stride {
                growths += 1;
                previous_stride = planar.stride();
            }
            let value = pcm_ramp[frames - 1];
            planar.channel_mut(0).expect("left channel exists")[frames - 1] = value;
            planar.channel_mut(1).expect("right channel exists")[frames - 1] =
                negative_pcm_ramp[frames - 1];
        }

        assert!(growths <= 9, "129 appends required {growths} stride moves");
        for frame in 1..=129 {
            let value = f32::from(u16::try_from(frame).expect("fixture frame fits u16"));
            assert_eq!(
                planar.channel(0).expect("left channel exists")[frame - 1],
                value
            );
            assert_eq!(
                planar.channel(1).expect("right channel exists")[frame - 1],
                -value
            );
        }
    }

    #[kithara::test]
    fn exact_budget_growth_is_not_rejected_by_amortization() {
        let exact_samples = 18;
        let pools = pools_with_budget(exact_samples * size_of::<f32>());
        let mut planar = PlanarBuffer::new(&pools, stereo(), FrameCount::new(8))
            .expect("initial planar storage fits");

        planar
            .resize_frames(FrameCount::new(9))
            .expect("exact requested shape fits the region budget");

        assert_eq!(planar.frames(), FrameCount::new(9));
        assert_eq!(planar.stride(), FrameCount::new(9));
    }

    #[kithara::test]
    fn view_ranges_and_channels_are_checked() {
        let pools = pools_with_budget(64 * size_of::<f32>());
        let planar =
            PlanarBuffer::new(&pools, stereo(), FrameCount::new(3)).expect("planar storage fits");

        assert_eq!(
            planar.view().range(2..4).map(|view| view.frames()),
            Err(SignalError::FrameRange {
                start: 2,
                end: 4,
                frames: 3,
            })
        );
        assert_eq!(
            planar.channel(2).map(<[f32]>::len),
            Err(SignalError::ChannelRange {
                channel: 2,
                channels: 2,
            })
        );
    }

    #[kithara::test]
    fn caller_and_pool_capacity_failures_are_typed() {
        let region = pools_with_budget(64 * size_of::<f32>());
        let planar =
            PlanarBuffer::new(&region, stereo(), FrameCount::new(2)).expect("planar storage fits");
        assert_eq!(
            planar.view().interleave_into(&mut [0.0; 3]),
            Err(SignalError::Capacity {
                required_samples: 4,
                available_samples: 3,
            })
        );

        let exhausted = pools_with_budget(0);
        assert!(matches!(
            PlanarBuffer::new(&exhausted, stereo(), FrameCount::new(2)),
            Err(SignalError::PoolCapacity {
                required_samples: 4
            })
        ));
    }

    #[kithara::test]
    fn dropped_planar_storage_is_reused_by_the_injected_pool() {
        let pools = pools_with_budget(64 * size_of::<f32>());
        let first = PlanarBuffer::new(&pools, stereo(), FrameCount::new(8))
            .expect("first planar storage fits");
        let ptr = first.as_samples().as_ptr();
        drop(first);

        let reused = PlanarBuffer::new(&pools, stereo(), FrameCount::new(8))
            .expect("reused planar storage fits");
        assert_eq!(reused.as_samples().as_ptr(), ptr);
    }
    #[kithara::test]
    fn fixed_shape_releases_oversized_recycled_capacity() {
        let pools = pools_with_budget(64 * size_of::<f32>());
        let large =
            PlanarBuffer::new(&pools, stereo(), FrameCount::new(32)).expect("large buffer fits");
        drop(large);
        let mut small = PlanarBuffer::new(&pools, stereo(), FrameCount::new(8))
            .expect("small buffer reuses allocation");
        assert_eq!(small.samples.capacity(), 64);
        small.channel_mut(1).expect("right channel").fill(0.5);
        small.shrink_to_fit();
        assert_eq!(small.stride(), FrameCount::new(8));
        assert_eq!(small.channel(1).expect("right channel"), &[0.5; 8]);
        let sibling = PlanarBuffer::new(&pools, stereo(), FrameCount::new(24))
            .expect("released capacity admits sibling under the unchanged budget");
        assert_eq!(sibling.frames(), FrameCount::new(24));
    }

    #[kithara::test]
    fn shrinking_stride_preserves_each_logical_channel() {
        let pools = pools_with_budget(64 * size_of::<f32>());
        let mut buffer =
            PlanarBuffer::new(&pools, stereo(), FrameCount::new(8)).expect("prepared storage");
        buffer.channel_mut(0).expect("left channel").fill(0.25);
        buffer.channel_mut(1).expect("right channel").fill(0.5);
        buffer
            .resize_frames(FrameCount::new(3))
            .expect("shorter span");
        buffer.shrink_to_fit();
        assert_eq!(buffer.stride(), FrameCount::new(3));
        assert_eq!(buffer.as_samples(), &[0.25, 0.25, 0.25, 0.5, 0.5, 0.5]);
        buffer
            .resize_frames(FrameCount::new(0))
            .expect("empty span");
        buffer.shrink_to_fit();
        assert_eq!(buffer.stride(), FrameCount::new(0));
        assert!(buffer.as_samples().is_empty());
    }
}
