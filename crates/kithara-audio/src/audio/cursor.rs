use std::{num::NonZeroUsize, ops::Range};

use kithara_platform::time::Duration;
use kithara_signal::{
    AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView, SignalError,
};
use kithara_stream::PlayheadWrite;
use kithara_test_utils::kithara;

use super::{
    ConsumerPhase, DecodeError, PendingReason, ReadOutcome, chunk_position,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer, Wait},
};
use crate::SourceSpan;

#[derive(Clone, Copy)]
pub(super) struct CursorRead {
    pub(super) first_output_meta: Option<AudioChunkInfo>,
    pub(super) outcome: ReadOutcome,
}

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

    pub(super) const fn begin_chunk(&mut self, chunk: &AudioChunk) {
        self.spec = chunk.spec();
        self.current_chunk_consumed_frames = 0;
    }

    pub(super) const fn clear(&mut self) {
        self.current_chunk_consumed_frames = 0;
    }

    fn copy_into(
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

    #[kithara::measure]
    pub(super) fn read(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        buf: &mut [f32],
    ) -> Result<CursorRead, DecodeError> {
        self.read_into(ring, events, playhead, recv, ReadBuffer::Interleaved(buf))
    }

    #[kithara::hang_watchdog]
    fn read_into(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        mut output: ReadBuffer<'_, '_>,
    ) -> Result<CursorRead, DecodeError> {
        let capacity = output.capacity()?;
        if capacity == 0 {
            return Ok(pending(playhead, PendingReason::Buffering));
        }
        match ring.phase {
            ConsumerPhase::AtEof if ring.current_chunk.is_none() => {
                return Ok(eof(playhead));
            }
            ConsumerPhase::Failed { source } => {
                return Err(DecodeError::audio_stream("cursor read", source));
            }
            _ => {}
        }

        let mut written = 0;
        let mut first_output_meta = None;
        let mut source_span = None;
        let mut source_output_frames = 0_u64;
        while written < capacity {
            hang_tick!();

            if let Some(chunk) = ring.current_chunk.as_ref() {
                let chunk_source_span = ring.current_source_span;
                if written > 0
                    && !source_spans_coalesce(
                        source_span,
                        source_output_frames,
                        chunk_source_span,
                        u64::from(chunk.meta.frames),
                    )
                {
                    break;
                }
                let copied =
                    self.copy_into(chunk, chunk_source_span, &mut output, written, playhead)?;
                if copied.count > 0 {
                    hang_reset!();
                    first_output_meta.get_or_insert(chunk.meta);
                    written += copied.count;
                    if let Some(next) = copied.source_span {
                        source_span =
                            source_span.map_or(Some(next), |current| current.followed_by(next));
                        source_output_frames = source_output_frames
                            .checked_add(copied.output_frames)
                            .ok_or(DecodeError::SampleCountOverflow {
                                frames: source_output_frames,
                                channels: 1,
                            })?;
                    }
                }
                if copied.finished {
                    ring.recycle_current();
                } else if copied.count == 0 {
                    break;
                }
            }

            if written >= capacity {
                break;
            }
            let was_playing = ring.phase == ConsumerPhase::Playing;
            let filled = ring.fill(self, recv, Wait::ForProducer);
            events.fill_result(
                filled,
                was_playing,
                ring.phase.is_terminal(),
                playhead.position(),
                ring.validator.epoch,
            );
            if !filled {
                break;
            }
        }

        if let Some(count) = NonZeroUsize::new(written) {
            let position = playhead.position();
            debug_assert!(count.get() <= capacity);
            debug_assert!(
                playhead
                    .duration()
                    .is_none_or(|duration| position <= duration)
            );
            return Ok(CursorRead {
                first_output_meta,
                outcome: ReadOutcome::Frames {
                    count,
                    position,
                    source_span,
                },
            });
        }

        Ok(match ring.phase {
            ConsumerPhase::AtEof => eof(playhead),
            ConsumerPhase::Failed { source } => {
                return Err(DecodeError::audio_stream("cursor read", source));
            }
            ConsumerPhase::SeekPending { .. } => pending(playhead, PendingReason::SeekInProgress),
            _ => pending(playhead, PendingReason::Buffering),
        })
    }

    pub(super) fn read_planar<'a>(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<CursorRead, DecodeError> {
        self.read_into(ring, events, playhead, recv, ReadBuffer::Planar(output))
    }
}

enum ReadBuffer<'a, 'b> {
    Interleaved(&'a mut [f32]),
    Planar(&'a mut [&'b mut [f32]]),
}

impl ReadBuffer<'_, '_> {
    fn capacity(&self) -> Result<usize, DecodeError> {
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

struct CopyOutcome {
    source_span: Option<SourceSpan>,
    finished: bool,
    output_frames: u64,
    count: usize,
}

fn source_spans_coalesce(
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

fn pending(playhead: &dyn PlayheadWrite, reason: PendingReason) -> CursorRead {
    CursorRead {
        outcome: ReadOutcome::Pending {
            reason,
            position: playhead.position(),
        },
        first_output_meta: None,
    }
}

fn eof(playhead: &dyn PlayheadWrite) -> CursorRead {
    CursorRead {
        outcome: ReadOutcome::Eof {
            position: playhead.position(),
        },
        first_output_meta: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::atomic::AtomicU64};

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_stream::PlayheadState;
    use kithara_test_fixtures::unit_fixtures::cursor_half;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode, SourceEnd,
        audio::{Fetch, ThreadWake, connect, ring::RingParts},
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
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(4, None);
        let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::data(chunk, 0))
            .expect("chunk reaches test ring");

        let playhead = PlayheadState::new();
        playhead.set_duration(Some(duration));
        let mut cursor = ChunkCursor::new(spec);
        let mut events = AudioEvents::test();
        let mut buf = vec![0.0; 200];
        let read = cursor
            .read(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut buf,
            )
            .expect("partial read succeeds");
        let ReadOutcome::Frames {
            count,
            position,
            source_span,
        } = read.outcome
        else {
            panic!("expected frames from partial resampled chunk");
        };
        assert_eq!(count.get(), 200);
        assert_eq!(position, duration);
        assert_eq!(source_span, None);
        assert_eq!(cursor.current_chunk_consumed_frames, 100);
    }

    #[kithara::test]
    fn reads_preserve_consecutive_rendered_source_spans_and_revisions(cursor_half: Vec<f32>) {
        let pools = pools();
        let rate = NonZeroU32::new(48_000).expect("test rate");
        let spec = AudioSpec::new(1, rate);
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(4, None);
        let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        let mut first = timed_chunk(
            &pools,
            &cursor_half,
            spec,
            3,
            Duration::ZERO,
            Duration::from_millis(3),
        );
        first.meta.frame_offset = 100;
        first.meta.render_revision = 7;
        let mut second = timed_chunk(
            &pools,
            &cursor_half,
            spec,
            2,
            Duration::from_millis(3),
            Duration::from_millis(5),
        );
        second.meta.frame_offset = 1_000;
        second.meta.render_revision = 7;
        let mut changed = timed_chunk(
            &pools,
            &cursor_half,
            spec,
            2,
            Duration::from_millis(5),
            Duration::from_millis(7),
        );
        changed.meta.frame_offset = 2_000;
        changed.meta.render_revision = 8;
        data_tx
            .try_push(Fetch::rendered(first, 0, SourceEnd::new(106, rate)))
            .expect("first rendered chunk reaches ring");
        data_tx
            .try_push(Fetch::rendered(second, 0, SourceEnd::new(110, rate)))
            .expect("second rendered chunk reaches ring");
        data_tx
            .try_push(Fetch::rendered(changed, 0, SourceEnd::new(114, rate)))
            .expect("changed-revision rendered chunk reaches ring");

        let playhead = PlayheadState::new();
        let mut cursor = ChunkCursor::new(spec);
        let mut events = AudioEvents::test();
        let mut output = [0.0; 8];
        let first_read = cursor
            .read(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut output,
            )
            .expect("first read succeeds");
        assert_eq!(
            first_read.first_output_meta.map(|meta| meta.timestamp),
            Some(Duration::ZERO)
        );
        let ReadOutcome::Frames {
            count, source_span, ..
        } = first_read.outcome
        else {
            panic!("expected first rendered frames");
        };
        assert_eq!(count.get(), 5);
        assert_eq!(
            source_span,
            SourceSpan::new(100, 110, rate, 5).map(|span| span.with_render_revision(7))
        );

        let second_read = cursor
            .read(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut output[..1],
            )
            .expect("partial changed-revision read succeeds");
        let ReadOutcome::Frames { source_span, .. } = second_read.outcome else {
            panic!("expected partial changed-revision frames");
        };
        assert_eq!(
            source_span,
            SourceSpan::new(110, 112, rate, 1).map(|span| span.with_render_revision(8))
        );

        let final_read = cursor
            .read(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut output,
            )
            .expect("final changed-revision read succeeds");
        let ReadOutcome::Frames { source_span, .. } = final_read.outcome else {
            panic!("expected final changed-revision frames");
        };
        assert_eq!(
            source_span,
            SourceSpan::new(112, 114, rate, 1).map(|span| span.with_render_revision(8))
        );
    }

    #[kithara::test]
    fn read_buffer_shorter_than_frame_preserves_current_chunk(cursor_half: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(1, None);
        let (trash_tx, mut trash_rx) = connect::<AudioChunk>(3, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::data(
                timed_chunk(
                    &pools,
                    &cursor_half,
                    spec,
                    1,
                    Duration::ZERO,
                    Duration::from_millis(1),
                ),
                0,
            ))
            .expect("chunk reaches test ring");
        let mut cursor = ChunkCursor::new(spec);
        let mut events = AudioEvents::test();
        let mut output = [0.0];

        let read = cursor
            .read(
                &mut ring,
                &mut events,
                &PlayheadState::new(),
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut output,
            )
            .expect("short read remains pending");

        assert!(matches!(read.outcome, ReadOutcome::Pending { .. }));
        assert!(ring.current_chunk.is_some());
        assert!(trash_rx.try_pop().is_none());
    }

    #[kithara::test]
    fn mono_planar_read_consumes_one_source_frame_per_output_frame() {
        let pools = pools();
        let (mut cursor, mut ring, mut events, playhead) = mono_ramp_cursor(&pools);
        let mut left = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut right = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut planar: [&mut [f32]; 2] = [&mut left, &mut right];

        let read = cursor
            .read_planar(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut planar,
            )
            .expect("mono planar read succeeds");

        let ReadOutcome::Frames { count, .. } = read.outcome else {
            panic!("expected frames from mono chunk");
        };
        assert_eq!(count.get(), consts::MONO_OUTPUT_FRAMES);
        assert_eq!(
            cursor.current_chunk_consumed_frames,
            consts::MONO_OUTPUT_FRAMES as u64,
            "a mono source read as interleaved stereo consumes two source frames per output frame, playing back at double rate"
        );
    }

    #[kithara::test]
    fn mono_planar_read_carries_each_sample_to_both_channels() {
        let pools = pools();
        let (mut cursor, mut ring, mut events, playhead) = mono_ramp_cursor(&pools);
        let mut left = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut right = vec![0.0; consts::MONO_OUTPUT_FRAMES];
        let mut planar: [&mut [f32]; 2] = [&mut left, &mut right];

        cursor
            .read_planar(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut planar,
            )
            .expect("mono planar read succeeds");

        let want: Vec<f32> = (0..consts::MONO_OUTPUT_FRAMES).map(|i| i as f32).collect();
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
    fn mono_ramp_cursor(pools: &Pools) -> (ChunkCursor, RingConsumer, AudioEvents, PlayheadState) {
        let spec = AudioSpec::new(1, NonZeroU32::new(48_000).expect("test rate"));
        let frames =
            u32::try_from(consts::MONO_OUTPUT_FRAMES * 2).expect("test frame count fits u32");
        let samples: Vec<f32> = (0..frames).map(|i| i as f32).collect();
        let chunk = AudioChunk::new(
            AudioChunkInfo {
                spec,
                timestamp: Duration::ZERO,
                end_timestamp: Duration::from_millis(1),
                frames,
                ..Default::default()
            },
            sample_buffer(pools, &samples),
        );
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(4, None);
        let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::data(chunk, 0))
            .expect("chunk reaches test ring");
        (
            ChunkCursor::new(spec),
            ring,
            AudioEvents::test(),
            PlayheadState::new(),
        )
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
    fn planar_read_crosses_chunks_but_preserves_mapping_boundaries() {
        let pools = pools();
        let rate = NonZeroU32::new(48_000).expect("test rate");
        let spec = AudioSpec::new(2, rate);
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(4, None);
        let (trash_tx, mut trash_rx) = connect::<AudioChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        let first_map = std::num::NonZeroU64::new(1);
        let next_map = std::num::NonZeroU64::new(2);
        for (offset, pcm, mapping) in [
            (0, [0.0, 10.0, 1.0, 11.0], first_map),
            (2, [2.0, 12.0, 3.0, 13.0], first_map),
            (4, [4.0, 14.0, 5.0, 15.0], next_map),
        ] {
            let mut chunk = timed_chunk(
                &pools,
                &pcm,
                spec,
                2,
                Duration::ZERO,
                Duration::from_millis(1),
            );
            chunk.meta.frame_offset = offset;
            chunk.meta.mapping_revision = mapping;
            data_tx
                .try_push(Fetch::rendered(chunk, 0, SourceEnd::new(offset + 2, rate)))
                .expect("rendered chunk reaches ring");
        }
        let mut cursor = ChunkCursor::new(spec);
        let mut events = AudioEvents::test();
        let playhead = PlayheadState::new();
        let mut left = [-1.0; 5];
        let mut right = [-1.0; 5];
        let read = cursor
            .read_planar(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut [&mut left, &mut right],
            )
            .expect("planar read succeeds");
        let ReadOutcome::Frames {
            count, source_span, ..
        } = read.outcome
        else {
            panic!("expected planar frames");
        };
        assert_eq!(count.get(), 4);
        assert_eq!(left, [0.0, 1.0, 2.0, 3.0, -1.0]);
        assert_eq!(right, [10.0, 11.0, 12.0, 13.0, -1.0]);
        assert_eq!(
            source_span,
            SourceSpan::new(0, 4, rate, 4).map(|span| span.with_mapping_revision(first_map))
        );
        assert!(trash_rx.try_pop().is_some());
        assert!(trash_rx.try_pop().is_some());
        assert!(trash_rx.try_pop().is_none());
        assert_eq!(
            ring.current_chunk
                .as_ref()
                .expect("next map remains resident")
                .meta
                .mapping_revision,
            next_map
        );
        assert_eq!(cursor.current_chunk_consumed_frames, 0);
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
        let (mut data_tx, data_rx) = connect::<Fetch<AudioChunk>>(4, None);
        let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            trash_tx,
            audio_rx: data_rx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
            consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::rendered(
                chunk,
                0,
                SourceEnd::new(origin + 192, rate),
            ))
            .expect("rendered chunk reaches ring");
        let mut cursor = ChunkCursor::new(spec);
        let mut events = AudioEvents::test();
        let playhead = PlayheadState::new();
        let mut left = [0.0; 127];
        let mut right = [0.0; 127];
        let read = cursor
            .read_planar(
                &mut ring,
                &mut events,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut [&mut left, &mut right],
            )
            .expect("partial read");
        let ReadOutcome::Frames {
            count,
            source_span: Some(source),
            ..
        } = read.outcome
        else {
            panic!("expected mapped PCM");
        };
        assert_eq!(count.get(), 127);
        assert_eq!(source.end(), origin + 190);
        let consumed = source.for_output_range(0..2).expect("consumer subrange");
        assert_eq!(consumed.end(), origin + 3);
        assert_eq!(consumed.render_revision(), 7);
        assert_eq!(consumed.mapping_revision(), mapping);
    }
}
