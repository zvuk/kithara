use std::{io::Error as IoError, num::NonZeroUsize};

use fast_interleave::deinterleave_variable;
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_stream::{ChunkPosition, PlayheadWrite};
use kithara_test_utils::kithara;

use super::{
    ConsumerPhase, DecodeError, PendingReason, ReadOutcome,
    ring::{RecvCtx, RingConsumer},
};

#[derive(Clone, Copy)]
pub(super) struct CursorRead {
    pub(super) outcome: ReadOutcome,
    pub(super) last_output_meta: Option<PcmMeta>,
}

pub(super) struct ChunkCursor {
    current_chunk_consumed_frames: u64,
    spec: PcmSpec,
    interleaved: Option<PcmBuf>,
}

impl ChunkCursor {
    pub(super) fn new(pool: &PcmPool, spec: PcmSpec) -> Self {
        let channels = usize::from(spec.channels).max(2);
        let sample_rate = usize::try_from(spec.sample_rate.get()).unwrap_or(usize::MAX);
        let capacity = sample_rate.saturating_mul(channels);
        let interleaved = pool.get_with(|buffer| {
            buffer.clear();
            let current = buffer.capacity();
            if current < capacity {
                buffer.reserve(capacity - current);
            }
        });
        Self {
            current_chunk_consumed_frames: 0,
            spec,
            interleaved: Some(interleaved),
        }
    }

    pub(super) fn spec(&self) -> PcmSpec {
        self.spec
    }

    pub(super) fn begin_chunk(&mut self, chunk: &PcmChunk) {
        self.spec = chunk.spec();
        self.current_chunk_consumed_frames = 0;
    }

    pub(super) fn clear(&mut self) {
        self.current_chunk_consumed_frames = 0;
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog]
    pub(super) fn read(
        &mut self,
        ring: &mut RingConsumer,
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
            ConsumerPhase::Failed => return Err(channel_failed()),
            _ => {}
        }

        let mut written = 0;
        let mut last_output_meta = None;
        while written < buf.len() {
            hang_tick!();

            if let Some(chunk) = ring.current_chunk.as_ref() {
                let copied = self.copy_into(chunk, &mut buf[written..], playhead)?;
                if copied.samples > 0 {
                    hang_reset!();
                    last_output_meta = Some(chunk.meta);
                    written += copied.samples;
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
            if !ring.fill(self, recv) {
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
                outcome: ReadOutcome::Frames { count, position },
                last_output_meta,
            });
        }

        Ok(match ring.phase {
            ConsumerPhase::AtEof => eof(playhead),
            ConsumerPhase::Failed => return Err(channel_failed()),
            ConsumerPhase::SeekPending { .. } => pending(playhead, PendingReason::SeekInProgress),
            _ => pending(playhead, PendingReason::Buffering),
        })
    }

    pub(super) fn read_planar<'a>(
        &mut self,
        ring: &mut RingConsumer,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<CursorRead, DecodeError> {
        let channels = output.len();
        let Some(num_channels) = NonZeroUsize::new(channels) else {
            return Ok(pending(playhead, PendingReason::Buffering));
        };
        let frames = output[0].len();
        let total_samples = frames * channels;
        let Some(mut interleaved) = self.interleaved.take() else {
            return Err(DecodeError::Io {
                source: IoError::other("interleaved scratch detached during planar read"),
            });
        };
        interleaved.clear();
        interleaved.ensure_len(total_samples)?;

        let result = self.read(ring, playhead, recv, &mut interleaved[..]);
        let result = match result {
            Ok(mut read) => {
                if let ReadOutcome::Frames { count, position } = read.outcome {
                    let actual_frames = count.get() / channels;
                    debug_assert!(actual_frames <= frames);
                    deinterleave_variable(&interleaved[..], num_channels, output, 0..actual_frames);
                    read.outcome = NonZeroUsize::new(actual_frames).map_or(
                        ReadOutcome::Pending {
                            position,
                            reason: PendingReason::Buffering,
                        },
                        |count| ReadOutcome::Frames { position, count },
                    );
                }
                Ok(read)
            }
            Err(error) => Err(error),
        };
        self.interleaved = Some(interleaved);
        result
    }

    fn copy_into(
        &mut self,
        chunk: &PcmChunk,
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
            });
        }

        let remaining_frames = total_frames - consumed;
        let output_frames = (output.len() as u64) / channels;
        let take_frames = remaining_frames.min(output_frames);
        if take_frames == 0 {
            return Ok(CopyOutcome {
                samples: 0,
                finished: false,
            });
        }

        let start_sample = frames_to_samples(consumed, channels)?;
        let samples = frames_to_samples(take_frames, channels)?;
        output[..samples].copy_from_slice(&chunk.samples[start_sample..start_sample + samples]);
        let consumed_total = consumed + take_frames;
        self.current_chunk_consumed_frames = consumed_total;
        let finished = take_frames == remaining_frames;
        if finished {
            playhead.advance(&ChunkPosition::from(&chunk.meta));
        } else {
            playhead.advance_partial(interpolated_position(chunk.meta, consumed_total));
        }
        Ok(CopyOutcome { samples, finished })
    }
}

struct CopyOutcome {
    samples: usize,
    finished: bool,
}

fn frames_to_samples(frames: u64, channels: u64) -> Result<usize, DecodeError> {
    let samples = frames.saturating_mul(channels);
    usize::try_from(samples).map_err(|error| DecodeError::Io {
        source: IoError::other(format!(
            "frames*channels overflow: {samples} does not fit usize: {error}"
        )),
    })
}

fn interpolated_position(meta: PcmMeta, consumed_frames: u64) -> kithara_platform::time::Duration {
    let total_frames = u64::from(meta.frames).max(1);
    let start_ns = u64::try_from(meta.timestamp.as_nanos()).unwrap_or(u64::MAX);
    let end_ns = u64::try_from(meta.end_timestamp.as_nanos()).unwrap_or(u64::MAX);
    let span_ns = u128::from(end_ns.saturating_sub(start_ns));
    let offset = span_ns * u128::from(consumed_frames) / u128::from(total_frames);
    let interpolated = u128::from(start_ns).saturating_add(offset);
    let nanos = u64::try_from(interpolated).unwrap_or(u64::MAX);
    kithara_platform::time::Duration::from_nanos(nanos)
}

fn channel_failed() -> DecodeError {
    DecodeError::Io {
        source: IoError::other("pcm channel closed / producer failed"),
    }
}

fn pending(playhead: &dyn PlayheadWrite, reason: PendingReason) -> CursorRead {
    CursorRead {
        outcome: ReadOutcome::Pending {
            reason,
            position: playhead.position(),
        },
        last_output_meta: None,
    }
}

fn eof(playhead: &dyn PlayheadWrite) -> CursorRead {
    CursorRead {
        outcome: ReadOutcome::Eof {
            position: playhead.position(),
        },
        last_output_meta: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::atomic::AtomicU64};

    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::PlayheadState;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::{Fetch, ThreadWake, connect, ring::RingParts};

    #[kithara::test]
    fn partial_resampled_chunk_position_caps_at_duration() {
        let spec = PcmSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let duration = Duration::from_nanos(36_360_000_000);
        let chunk = timed_chunk(
            spec,
            148,
            duration.saturating_sub(Duration::from_millis(2)),
            duration.saturating_add(Duration::from_millis(2)),
        );
        let (mut data_tx, data_rx) = connect::<Fetch<PcmChunk>>(4, None);
        let (trash_tx, _trash_rx) = connect::<PcmChunk>(8, None);
        let mut ring = RingConsumer::new(RingParts {
            pcm_rx: data_rx,
            trash_tx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::data(chunk, 0))
            .expect("chunk reaches test ring");

        let playhead = PlayheadState::new();
        playhead.set_duration(Some(duration));
        let pool = PcmPool::default().clone();
        let mut cursor = ChunkCursor::new(&pool, spec);
        let mut buf = vec![0.0; 200];
        let read = cursor
            .read(
                &mut ring,
                &playhead,
                RecvCtx {
                    cancel: None,
                    worker: None,
                    abr: None,
                },
                &mut buf,
            )
            .expect("partial read succeeds");
        let ReadOutcome::Frames { count, position } = read.outcome else {
            panic!("expected frames from partial resampled chunk");
        };
        assert_eq!(count.get(), 200);
        assert_eq!(position, duration);
        assert_eq!(cursor.current_chunk_consumed_frames, 100);
    }

    #[kithara::test]
    fn read_buffer_shorter_than_frame_preserves_current_chunk() {
        let spec = PcmSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let (mut data_tx, data_rx) = connect::<Fetch<PcmChunk>>(1, None);
        let (trash_tx, mut trash_rx) = connect::<PcmChunk>(3, None);
        let mut ring = RingConsumer::new(RingParts {
            pcm_rx: data_rx,
            trash_tx,
            reader_wake: Arc::new(ThreadWake::default()),
            epoch: Arc::new(AtomicU64::new(0)),
            block_on_underrun: false,
        });
        ring.preloaded = true;
        data_tx
            .try_push(Fetch::data(
                timed_chunk(spec, 1, Duration::ZERO, Duration::from_millis(1)),
                0,
            ))
            .expect("chunk reaches test ring");
        let pool = PcmPool::default().clone();
        let mut cursor = ChunkCursor::new(&pool, spec);
        let mut output = [0.0];

        let read = cursor
            .read(
                &mut ring,
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

    fn timed_chunk(spec: PcmSpec, frames: u32, start: Duration, end: Duration) -> PcmChunk {
        let channels = usize::from(spec.channels.max(1));
        let frame_count = usize::try_from(frames).expect("test frame count fits usize");
        let samples = vec![0.5; frame_count * channels];
        PcmChunk::new(
            PcmMeta {
                spec,
                timestamp: start,
                end_timestamp: end,
                frames,
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }
}
