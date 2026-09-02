use std::num::NonZeroUsize;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, InterleavedView};
use kithara_stream::PlayheadWrite;
use kithara_test_utils::kithara;

use super::{
    ConsumerPhase, DecodeError, PendingReason, ReadOutcome, chunk_position,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer},
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
    interleaved: Option<SampleBuffer>,
    current_chunk_consumed_frames: u64,
}

impl ChunkCursor {
    pub(super) fn new<S>(pools: &PoolRegion<S>, spec: AudioSpec) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let channels = usize::from(spec.channels).max(2);
        let sample_rate = usize::try_from(spec.sample_rate.get()).unwrap_or(usize::MAX);
        let capacity = sample_rate.saturating_mul(channels);
        let mut interleaved = pools.get_with_len::<f32>(capacity)?;
        interleaved.clear();
        Ok(Self {
            spec,
            current_chunk_consumed_frames: 0,
            interleaved: Some(interleaved),
        })
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
        output: &mut [f32],
        playhead: &dyn PlayheadWrite,
    ) -> Result<CopyOutcome, DecodeError> {
        let channels = u64::from(chunk.meta.spec.channels.max(1));
        let total_frames = u64::from(chunk.meta.frames);
        let consumed = self.current_chunk_consumed_frames;
        if consumed >= total_frames {
            return Ok(CopyOutcome {
                samples: 0,
                finished: true,
                output_frames: 0,
                source_span: None,
            });
        }

        let remaining_frames = total_frames - consumed;
        let output_frames = (output.len() as u64) / channels;
        let take_frames = remaining_frames.min(output_frames);
        if take_frames == 0 {
            return Ok(CopyOutcome {
                samples: 0,
                finished: false,
                output_frames: 0,
                source_span: None,
            });
        }

        let start_sample = frames_to_samples(consumed, channels)?;
        let samples = frames_to_samples(take_frames, channels)?;
        output[..samples].copy_from_slice(&chunk.samples[start_sample..start_sample + samples]);
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
            output_frames: take_frames,
            samples,
            source_span,
        })
    }

    #[kithara::measure]
    #[kithara::hang_watchdog]
    pub(super) fn read(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        buf: &mut [f32],
    ) -> Result<CursorRead, DecodeError> {
        if buf.is_empty() {
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
        while written < buf.len() {
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
                    self.copy_into(chunk, chunk_source_span, &mut buf[written..], playhead)?;
                if copied.samples > 0 {
                    hang_reset!();
                    first_output_meta.get_or_insert(chunk.meta);
                    written += copied.samples;
                    if let Some(next) = copied.source_span {
                        source_span = source_span.map_or(Some(next), |current| {
                            SourceSpan::new(current.start(), next.end(), current.sample_rate())
                                .map(|span| span.with_render_revision(current.render_revision()))
                        });
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
                } else if copied.samples == 0 {
                    break;
                }
            }

            if written >= buf.len() {
                break;
            }
            let was_playing = ring.phase == ConsumerPhase::Playing;
            let filled = ring.fill(self, recv);
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
            debug_assert!(count.get() <= buf.len());
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
        let channels = output.len();
        if channels == 0 {
            return Ok(pending(playhead, PendingReason::Buffering));
        }
        let frames = output[0].len();
        let total_samples =
            frames
                .checked_mul(channels)
                .ok_or_else(|| DecodeError::SampleCountOverflow {
                    frames: u64::try_from(frames).unwrap_or(u64::MAX),
                    channels: u64::try_from(channels).unwrap_or(u64::MAX),
                })?;
        let Some(mut interleaved) = self.interleaved.take() else {
            return Err(DecodeError::ScratchDetached);
        };
        interleaved.clear();
        interleaved.ensure_len(total_samples)?;

        let result = self.read(ring, events, playhead, recv, &mut interleaved[..]);
        let result = match result {
            Ok(mut read) => {
                if let ReadOutcome::Frames {
                    count,
                    position,
                    source_span,
                } = read.outcome
                {
                    let actual_frames = count.get() / channels;
                    debug_assert!(actual_frames <= frames);
                    let input = InterleavedView::new(
                        &interleaved[..count.get()],
                        self.spec,
                        FrameCount::new(actual_frames),
                    )?;
                    input.deinterleave_channels_into(output)?;
                    read.outcome = NonZeroUsize::new(actual_frames).map_or(
                        ReadOutcome::Pending {
                            position,
                            reason: PendingReason::Buffering,
                        },
                        |count| ReadOutcome::Frames {
                            position,
                            count,
                            source_span,
                        },
                    );
                }
                Ok(read)
            }
            Err(error) => Err(error),
        };
        self.interleaved = Some(interleaved);
        result
    }
}

struct CopyOutcome {
    finished: bool,
    output_frames: u64,
    samples: usize,
    source_span: Option<SourceSpan>,
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
    if current.end() != next.start()
        || current.sample_rate() != next.sample_rate()
        || current.render_revision() != next.render_revision()
        || current_output_frames == 0
        || next_output_frames == 0
    {
        return false;
    }
    let Some(current_source_frames) = current.end().checked_sub(current.start()) else {
        return false;
    };
    let Some(next_source_frames) = next.end().checked_sub(next.start()) else {
        return false;
    };
    u128::from(current_source_frames)
        .checked_mul(u128::from(next_output_frames))
        .zip(u128::from(next_source_frames).checked_mul(u128::from(current_output_frames)))
        .is_some_and(|(current_slope, next_slope)| current_slope == next_slope)
}

fn source_subspan(
    span: SourceSpan,
    output_start: u64,
    output_end: u64,
    output_frames: u64,
) -> Option<SourceSpan> {
    if output_frames == 0 || output_start > output_end || output_end > output_frames {
        return None;
    }
    let source_frames = span.end().checked_sub(span.start())?;
    let source_at = |output_frame: u64| {
        let offset = u128::from(source_frames)
            .checked_mul(u128::from(output_frame))?
            .checked_div(u128::from(output_frames))?;
        span.start().checked_add(u64::try_from(offset).ok()?)
    };
    let start = source_at(output_start)?;
    let end = if output_end == output_frames {
        span.end()
    } else {
        source_at(output_end)?
    };
    SourceSpan::new(start, end, span.sample_rate())
        .map(|subspan| subspan.with_render_revision(span.render_revision()))
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
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode, SourceEnd,
        audio::{Fetch, ThreadWake, connect, ring::RingParts},
        test_pools::{Pools, pools, sample_buffer},
    };

    #[kithara::test]
    fn partial_resampled_chunk_position_caps_at_duration() {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let duration = Duration::from_nanos(36_360_000_000);
        let chunk = timed_chunk(
            &pools,
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
        let mut cursor = ChunkCursor::new(&pools, spec).expect("cursor scratch fits test pools");
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
    fn render_revision_splits_identical_empty_source_spans() {
        let rate = NonZeroU32::new(48_000).expect("test rate");
        let first = SourceSpan::new(100, 100, rate)
            .expect("ordered test span")
            .with_render_revision(7);
        let same_revision = SourceSpan::new(100, 100, rate)
            .expect("ordered test span")
            .with_render_revision(7);
        let next_revision = SourceSpan::new(100, 100, rate)
            .expect("ordered test span")
            .with_render_revision(8);

        assert!(source_spans_coalesce(
            Some(first),
            64,
            Some(same_revision),
            64
        ));
        assert!(!source_spans_coalesce(
            Some(first),
            64,
            Some(next_revision),
            64
        ));
    }

    #[kithara::test]
    fn reads_preserve_each_rendered_source_span() {
        let pools = pools();
        let rate = NonZeroU32::new(48_000).expect("test rate");
        let first_revision = 7;
        let second_revision = 8;
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
        let mut first = timed_chunk(&pools, spec, 3, Duration::ZERO, Duration::from_millis(3));
        first.meta.frame_offset = 100;
        first.meta.render_revision = first_revision;
        let mut second = timed_chunk(
            &pools,
            spec,
            2,
            Duration::from_millis(3),
            Duration::from_millis(5),
        );
        second.meta.frame_offset = 106;
        second.meta.render_revision = first_revision;
        let mut changed = timed_chunk(
            &pools,
            spec,
            2,
            Duration::from_millis(5),
            Duration::from_millis(7),
        );
        changed.meta.frame_offset = 110;
        changed.meta.render_revision = second_revision;
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
        let mut cursor = ChunkCursor::new(&pools, spec).expect("cursor scratch fits test pools");
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
        let ReadOutcome::Frames {
            count, source_span, ..
        } = first_read.outcome
        else {
            panic!("expected first rendered frames");
        };
        assert_eq!(
            count.get(),
            5,
            "equal-revision Fetch spans must coalesce and a new render revision must split"
        );
        assert_eq!(
            source_span,
            SourceSpan::new(100, 110, rate).map(|span| span.with_render_revision(first_revision))
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
            .expect("partial changed-slope read succeeds");
        let ReadOutcome::Frames { source_span, .. } = second_read.outcome else {
            panic!("expected partial changed-revision frames");
        };
        assert_eq!(
            source_span,
            SourceSpan::new(110, 112, rate).map(|span| span.with_render_revision(second_revision))
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
            .expect("final changed-slope read succeeds");
        let ReadOutcome::Frames { source_span, .. } = final_read.outcome else {
            panic!("expected final rendered frames");
        };
        assert_eq!(
            source_span,
            SourceSpan::new(112, 114, rate).map(|span| span.with_render_revision(second_revision)),
            "a full changed-revision read must land exactly at SourceEnd"
        );
    }

    #[kithara::test]
    fn read_buffer_shorter_than_frame_preserves_current_chunk() {
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
                timed_chunk(&pools, spec, 1, Duration::ZERO, Duration::from_millis(1)),
                0,
            ))
            .expect("chunk reaches test ring");
        let mut cursor = ChunkCursor::new(&pools, spec).expect("cursor scratch fits test pools");
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

    fn timed_chunk(
        pools: &Pools,
        spec: AudioSpec,
        frames: u32,
        start: Duration,
        end: Duration,
    ) -> AudioChunk {
        let channels = usize::from(spec.channels.max(1));
        let frame_count = usize::try_from(frames).expect("test frame count fits usize");
        let samples = vec![0.5; frame_count * channels];
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                timestamp: start,
                end_timestamp: end,
                frames,
                ..Default::default()
            },
            sample_buffer(pools, &samples),
        )
    }
}
