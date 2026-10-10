use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_signal::{
    AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, SignalError,
};
use kithara_stream::PlayheadWrite;

use super::chunk_position;
use crate::{DecodeError, SourceSpan};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct ChunkCursor {
    #[field(get, vis = "pub(super)", copy)]
    spec: AudioSpec,
    current_chunk_consumed_frames: u64,
}

impl ChunkCursor {
    pub(super) const fn new(spec: AudioSpec) -> Self {
        Self {
            spec,
            current_chunk_consumed_frames: 0,
        }
    }

    pub(super) const fn set_spec(&mut self, spec: AudioSpec) {
        self.spec = spec;
    }

    pub(super) const fn begin_chunk(&mut self, chunk: &AudioChunk) {
        self.spec = chunk.spec();
        self.current_chunk_consumed_frames = 0;
    }

    delegate::delegate! {
        to self {
            #[expr(self.current_chunk_consumed_frames)]
            pub(super) const fn consumed_frames(&self) -> u64;
        }
    }

    pub(super) const fn clear(&mut self) {
        self.current_chunk_consumed_frames = 0;
    }

    pub(super) fn copy_into(
        &mut self,
        chunk: &AudioChunk,
        source_span: Option<SourceSpan>,
        output: &mut ReadBuffer<'_, '_>,
        written: usize,
        playhead: &dyn PlayheadWrite,
    ) -> Result<CopyOutcome, DecodeError> {
        let channels = u64::from(chunk.meta.spec.channels.max(1));
        let total_frames = u64::from(chunk.meta.frames);
        let consumed = self.current_chunk_consumed_frames;
        if consumed >= total_frames {
            return Ok(CopyOutcome {
                count: 0,
                finished: true,
                output_frames: 0,
                source_span: None,
            });
        }

        let remaining_frames = total_frames - consumed;
        let output_frames =
            u64::try_from(output.remaining_frames(written, channels)?).map_err(|_| {
                DecodeError::SampleCountOverflow {
                    channels,
                    frames: u64::MAX,
                }
            })?;
        let take_frames = remaining_frames.min(output_frames);
        if take_frames == 0 {
            return Ok(CopyOutcome {
                count: 0,
                finished: false,
                output_frames: 0,
                source_span: None,
            });
        }

        let start_sample = frames_to_samples(consumed, channels)?;
        let samples = frames_to_samples(take_frames, channels)?;
        let view = InterleavedView::new(
            &chunk.samples[start_sample..start_sample + samples],
            chunk.spec(),
            FrameCount::new(usize::try_from(take_frames).map_err(|_| {
                DecodeError::SampleCountOverflow {
                    channels,
                    frames: take_frames,
                }
            })?),
        )?;
        let count = output.copy_from(view, written)?;
        let consumed_total = consumed + take_frames;
        self.current_chunk_consumed_frames = consumed_total;
        let finished = take_frames == remaining_frames;
        let source_span = source_span
            .and_then(|span| source_subspan(span, consumed, consumed_total, total_frames));
        if finished {
            playhead.advance(&chunk_position(&chunk.meta));
        } else {
            playhead.advance_partial(interpolated_position(chunk.meta, consumed_total));
        }
        Ok(CopyOutcome {
            finished,
            count,
            source_span,
            output_frames: take_frames,
        })
    }
}

pub(super) enum ReadBuffer<'a, 'b> {
    Interleaved(&'a mut [f32]),
    Planar(&'a mut [&'b mut [f32]]),
}

impl ReadBuffer<'_, '_> {
    pub(super) fn capacity(&self) -> Result<usize, DecodeError> {
        match self {
            Self::Interleaved(output) => Ok(output.len()),
            Self::Planar(output) => {
                let frames = output.first().map_or(0, |plane| plane.len());
                for (channel, plane) in output.iter().enumerate().skip(1) {
                    if plane.len() != frames {
                        return Err(SignalError::ChannelFrames {
                            channel,
                            expected: frames,
                            actual: plane.len(),
                        }
                        .into());
                    }
                }
                Ok(frames)
            }
        }
    }

    fn copy_from(
        &mut self,
        source: InterleavedView<'_>,
        written: usize,
    ) -> Result<usize, DecodeError> {
        match self {
            Self::Interleaved(output) => {
                let samples = source.samples();
                output[written..written + samples.len()].copy_from_slice(samples);
                Ok(samples.len())
            }
            Self::Planar(output) => {
                let channels = source.spec().channel_count()?.get();
                let planes = output.len();
                let (filled, unfilled) = output.split_at_mut(channels.min(planes));
                let range = written..written + source.frames().get();
                source.deinterleave_channels_into_at(filled, written)?;
                spread_leading_channel(filled, unfilled, range);
                Ok(source.frames().get())
            }
        }
    }

    fn remaining_frames(&self, written: usize, channels: u64) -> Result<usize, DecodeError> {
        let remaining = self.capacity()?.saturating_sub(written);
        match self {
            Self::Interleaved(_) => {
                let channels =
                    usize::try_from(channels).map_err(|_| DecodeError::SampleCountOverflow {
                        channels,
                        frames: 0,
                    })?;
                Ok(remaining / channels)
            }
            Self::Planar(_) => Ok(remaining),
        }
    }
}

pub(super) struct CopyOutcome {
    pub(super) source_span: Option<SourceSpan>,
    pub(super) finished: bool,
    pub(super) output_frames: u64,
    pub(super) count: usize,
}

pub(super) fn source_spans_coalesce(
    current: Option<SourceSpan>,
    current_output_frames: u64,
    next: Option<SourceSpan>,
    next_output_frames: u64,
) -> bool {
    let (Some(current), Some(next)) = (current, next) else {
        return current.is_none() && next.is_none();
    };
    current.output_frames() == current_output_frames
        && next.output_frames() == next_output_frames
        && current.followed_by(next).is_some()
}

fn source_subspan(
    span: SourceSpan,
    output_start: u64,
    output_end: u64,
    output_frames: u64,
) -> Option<SourceSpan> {
    (span.output_frames() == output_frames)
        .then(|| span.for_output_range(output_start..output_end))
        .flatten()
}

/// Copy the leading source channel into the output planes the stream leaves
/// untouched, so a mono stream reaches every plane of a stereo device.
fn spread_leading_channel(filled: &[&mut [f32]], unfilled: &mut [&mut [f32]], range: Range<usize>) {
    let Some(leading) = filled.first() else {
        return;
    };
    let leading = &leading[range.clone()];
    for plane in unfilled {
        plane[range.clone()].copy_from_slice(leading);
    }
}

fn frames_to_samples(frames: u64, channels: u64) -> Result<usize, DecodeError> {
    usize::try_from(frames.saturating_mul(channels))
        .map_err(|_| DecodeError::SampleCountOverflow { frames, channels })
}

fn interpolated_position(meta: AudioChunkInfo, consumed_frames: u64) -> Duration {
    let total_frames = u64::from(meta.frames).max(1);
    let start_ns = u64::try_from(meta.timestamp.as_nanos()).unwrap_or(u64::MAX);
    let end_ns = u64::try_from(meta.end_timestamp.as_nanos()).unwrap_or(u64::MAX);
    let span_ns = u128::from(end_ns.saturating_sub(start_ns));
    let offset = span_ns * u128::from(consumed_frames) / u128::from(total_frames);
    let interpolated = u128::from(start_ns).saturating_add(offset);
    let nanos = u64::try_from(interpolated).unwrap_or(u64::MAX);
    Duration::from_nanos(nanos)
}
#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::time::Duration;
    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_stream::{PlayheadRead, PlayheadState};
    use kithara_test_fixtures::unit_fixtures::cursor_half;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        consts,
        test_pools::{Pools, pools, sample_buffer},
    };

    #[kithara::test]
    fn partial_resampled_chunk_position_caps_at_duration(cursor_half: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let duration = Duration::from_nanos(36_360_000_000);
        let chunk = timed_chunk(
            &pools,
            &cursor_half,
            spec,
            148,
            duration.saturating_sub(Duration::from_millis(2)),
            duration.saturating_add(Duration::from_millis(2)),
        );
        let playhead = PlayheadState::new();
        playhead.set_duration(Some(duration));
        let mut cursor = ChunkCursor::new(spec);
        let mut buf = vec![0.0; 200];
        let read = cursor
            .copy_into(
                &chunk,
                None,
                &mut ReadBuffer::Interleaved(&mut buf),
                0,
                &playhead,
            )
            .expect("partial read succeeds");
        let count = read.count;
        let position = playhead.position();
        let source_span = read.source_span;
        assert_eq!(count, 200);
        assert_eq!(position, duration);
        assert_eq!(source_span, None);
        assert_eq!(cursor.current_chunk_consumed_frames, 100);
    }

    #[kithara::test]
    fn read_buffer_shorter_than_frame_preserves_current_chunk(cursor_half: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let chunk = timed_chunk(
            &pools,
            &cursor_half,
            spec,
            1,
            Duration::ZERO,
            Duration::from_millis(1),
        );
        let mut cursor = ChunkCursor::new(spec);
        let mut output = [0.0];
        let read = cursor
            .copy_into(
                &chunk,
                None,
                &mut ReadBuffer::Interleaved(&mut output),
                0,
                &PlayheadState::new(),
            )
            .expect("short read remains pending");
        assert_eq!(read.count, 0);
        assert!(!read.finished);
        assert_eq!(cursor.current_chunk_consumed_frames, 0);
        assert_eq!(&*chunk.samples, &cursor_half[..2]);
    }

    #[kithara::test]
    fn mono_planar_read_consumes_one_source_frame_per_output_frame() {
        let pools = pools();
        let (mut cursor, chunk, playhead) = mono_ramp_cursor(&pools);
        let mut left = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut right = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut planar: [&mut [f32]; 2] = [&mut left, &mut right];

        let read = cursor
            .copy_into(
                &chunk,
                None,
                &mut ReadBuffer::Planar(&mut planar),
                0,
                &playhead,
            )
            .expect("mono planar read succeeds");

        assert_eq!(read.count, consts::MONO_OUTPUT_FRAMES);
        assert_eq!(
            cursor.current_chunk_consumed_frames,
            consts::MONO_OUTPUT_FRAMES as u64,
            "a mono source read as interleaved stereo consumes two source frames per output frame, playing back at double rate"
        );
    }

    #[kithara::test]
    fn mono_planar_read_carries_each_sample_to_both_channels() {
        let pools = pools();
        let (mut cursor, chunk, playhead) = mono_ramp_cursor(&pools);
        let mut left = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut right = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut planar: [&mut [f32]; 2] = [&mut left, &mut right];

        cursor
            .copy_into(
                &chunk,
                None,
                &mut ReadBuffer::Planar(&mut planar),
                0,
                &playhead,
            )
            .expect("mono planar read succeeds");

        let frames = u16::try_from(consts::MONO_OUTPUT_FRAMES).expect("test frame count fits u16");
        let want: Vec<f32> = (0..frames).map(f32::from).collect();
        assert_eq!(
            left, want,
            "mono frames must reach the left channel in source order"
        );
        assert_eq!(
            right, want,
            "mono frames must reach the right channel in source order"
        );
    }

    /// A cursor over one mono chunk whose samples ramp `0.0, 1.0, ...` so a
    /// misread of the interleave shows up as a gap in the recovered order.
    fn mono_ramp_cursor(pools: &Pools) -> (ChunkCursor, AudioChunk, PlayheadState) {
        let spec = AudioSpec::new(1, NonZeroU32::new(48_000).expect("test rate"));
        let frames =
            u16::try_from(consts::MONO_OUTPUT_FRAMES * 2).expect("test frame count fits u16");
        let samples: Vec<f32> = (0..frames).map(f32::from).collect();
        let chunk = AudioChunk::new(
            AudioChunkInfo {
                spec,
                timestamp: Duration::ZERO,
                end_timestamp: Duration::from_millis(1),
                frames: u32::from(frames),
                ..Default::default()
            },
            sample_buffer(pools, &samples),
        );
        (ChunkCursor::new(spec), chunk, PlayheadState::new())
    }

    fn timed_chunk(
        pools: &Pools,
        pcm: &[f32],
        spec: AudioSpec,
        frames: u32,
        start: Duration,
        end: Duration,
    ) -> AudioChunk {
        let channels = usize::from(spec.channels.max(1));
        let frame_count = usize::try_from(frames).expect("test frame count fits usize");
        let samples = &pcm[..frame_count * channels];
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                timestamp: start,
                end_timestamp: end,
                frames,
                ..Default::default()
            },
            sample_buffer(pools, samples),
        )
    }

    #[kithara::test]
    fn mapping_revision_boundaries_survive_source_span_slicing() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let first_map = std::num::NonZeroU64::new(1);
        let next_map = std::num::NonZeroU64::new(2);
        let first =
            SourceSpan::new(0, 128, rate, 128).map(|span| span.with_mapping_revision(first_map));
        let next =
            SourceSpan::new(128, 256, rate, 128).map(|span| span.with_mapping_revision(next_map));
        assert!(!source_spans_coalesce(first, 128, next, 128));
        let first = first.expect("ordered source interval");
        let sliced = source_subspan(first, 16, 32, 128).expect("valid slice");
        assert_eq!(sliced.start(), 16);
        assert_eq!(sliced.end(), 32);
        assert_eq!(sliced.mapping_revision(), first_map);
    }
    #[kithara::test]
    #[case::zero_origin(0)]
    #[case::nonzero_origin(100)]
    fn planar_partial_read_preserves_source_basis_for_later_consumption(#[case] origin: u64) {
        let pools = pools();
        let rate = NonZeroU32::new(48_000).expect("rate");
        let spec = AudioSpec {
            channels: 2,
            sample_rate: rate,
        };
        let mapping = std::num::NonZeroU64::new(3);
        let mut chunk = timed_chunk(
            &pools,
            &[0.25; 256],
            spec,
            128,
            Duration::ZERO,
            Duration::from_millis(4),
        );
        chunk.meta.frame_offset = origin;
        chunk.meta.render_revision = 7;
        chunk.meta.mapping_revision = mapping;
        let span = SourceSpan::new(origin, origin + 192, rate, 128)
            .map(|span| span.with_render_revision(7).with_mapping_revision(mapping));
        let mut cursor = ChunkCursor::new(spec);
        let playhead = PlayheadState::new();
        let mut left = [0.0; 127];
        let mut right = [0.0; 127];
        let mut output = [&mut left[..], &mut right[..]];
        let read = cursor
            .copy_into(
                &chunk,
                span,
                &mut ReadBuffer::Planar(&mut output),
                0,
                &playhead,
            )
            .expect("partial read");
        let count = read.count;
        let source = read.source_span.expect("expected mapped PCM");
        assert_eq!(count, 127);
        assert_eq!(source.end(), origin + 190);
        let consumed = source.for_output_range(0..2).expect("consumer subrange");
        assert_eq!(consumed.end(), origin + 3);
        assert_eq!(consumed.render_revision(), 7);
        assert_eq!(consumed.mapping_revision(), mapping);
    }
}
