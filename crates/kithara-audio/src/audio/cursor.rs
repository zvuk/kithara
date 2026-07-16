use std::{io::Error as IoError, num::NonZeroUsize};

use fast_interleave::deinterleave_variable;
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_stream::{ChunkPosition, PlayheadWrite};
use kithara_test_utils::kithara;
use num_traits::cast::ToPrimitive;

use super::{
    ConsumerPhase, DecodeError, PendingReason, ReadOutcome,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer},
};
mod varispeed;

const NANOS_PER_SECOND_F64: f64 = 1_000_000_000.0;

enum VarispeedReadiness {
    FinishCurrent,
    Pending,
    Ready,
}

#[derive(Clone, Copy)]
pub(super) struct CursorRead {
    pub(super) outcome: ReadOutcome,
    pub(super) last_output_meta: Option<PcmMeta>,
}

pub(super) struct ChunkCursor {
    current_chunk_consumed_frames: u64,
    source_cursor: f64,
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
            source_cursor: 0.0,
            spec,
            interleaved: Some(interleaved),
        }
    }

    pub(super) fn spec(&self) -> PcmSpec {
        self.spec
    }

    pub(super) fn begin_chunk(&mut self, chunk: &PcmChunk) {
        self.begin_chunk_at(chunk, 0.0);
    }

    pub(super) fn clear(&mut self) {
        self.current_chunk_consumed_frames = 0;
        self.source_cursor = 0.0;
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(super) fn read(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        pitch_bend: f32,
        buf: &mut [f32],
    ) -> Result<CursorRead, DecodeError> {
        if buf.is_empty() {
            return Ok(pending(playhead, PendingReason::Buffering));
        }
        match ring.phase {
            ConsumerPhase::AtEof if ring.current_chunk.is_none() && ring.lookahead.is_none() => {
                return Ok(eof(playhead));
            }
            ConsumerPhase::Failed => return Err(channel_failed()),
            _ => {}
        }

        let cursor_is_integer = self.source_cursor
            == self
                .current_chunk_consumed_frames
                .to_f64()
                .unwrap_or(f64::INFINITY);
        let progress = if pitch_bend == 1.0 && cursor_is_integer {
            self.read_passthrough(ring, events, playhead, recv, buf)?
        } else {
            self.read_varispeed(ring, events, playhead, recv, buf, pitch_bend)?
        };
        let ReadProgress {
            samples: written,
            last_output_meta,
        } = progress;

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

    fn begin_chunk_at(&mut self, chunk: &PcmChunk, source_cursor: f64) {
        self.spec = chunk.spec();
        self.source_cursor = source_cursor.max(0.0);
        self.sync_consumed_frames(u64::from(chunk.meta.frames));
    }

    fn sync_consumed_frames(&mut self, total_frames: u64) {
        let consumed = self.source_cursor.floor().to_u64().unwrap_or(u64::MAX);
        self.current_chunk_consumed_frames = consumed.min(total_frames);
    }

    fn fill(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
    ) -> bool {
        let was_playing = ring.phase == ConsumerPhase::Playing;
        let filled = ring.fill(self, recv);
        events.fill_result(
            filled,
            was_playing,
            ring.phase.is_terminal(),
            playhead.position(),
            ring.validator.epoch,
        );
        filled
    }

    #[kithara::hang_watchdog]
    fn read_passthrough(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        buf: &mut [f32],
    ) -> Result<ReadProgress, DecodeError> {
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

            if written >= buf.len() || !self.fill(ring, events, playhead, recv) {
                break;
            }
        }
        Ok(ReadProgress {
            samples: written,
            last_output_meta,
        })
    }

    #[kithara::hang_watchdog]
    fn read_varispeed(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        buf: &mut [f32],
        requested_multiplier: f32,
    ) -> Result<ReadProgress, DecodeError> {
        let mut written = 0;
        let mut last_output_meta = None;
        while written < buf.len() {
            hang_tick!();

            if ring.current_chunk.is_none() && !self.fill(ring, events, playhead, recv) {
                break;
            }

            let Some((channels, channel_samples, total_frames, multiplier, meta)) =
                ring.current_chunk.as_ref().map(|chunk| {
                    let channels = u64::from(chunk.meta.spec.channels.max(1));
                    (
                        channels,
                        frames_to_samples(1, channels),
                        u64::from(chunk.meta.frames),
                        Self::effective_multiplier(requested_multiplier, chunk),
                        chunk.meta,
                    )
                })
            else {
                break;
            };
            let channel_samples = channel_samples?;
            if buf.len() - written < channel_samples {
                break;
            }

            if total_frames == 0 {
                self.finish_current(ring, playhead, 0.0);
                continue;
            }
            if self.source_cursor_at_or_past(total_frames) {
                let residual = self.residual_after(total_frames);
                self.finish_current(ring, playhead, residual);
                continue;
            }

            match self.varispeed_readiness(ring, events, playhead, recv, total_frames, multiplier) {
                VarispeedReadiness::Ready => {}
                VarispeedReadiness::Pending => break,
                VarispeedReadiness::FinishCurrent => {
                    let residual = self.residual_after(total_frames);
                    self.finish_current(ring, playhead, residual);
                    continue;
                }
            }

            hang_reset!();
            last_output_meta = Some(meta);
            self.write_varispeed_frame(
                ring,
                &mut buf[written..written + channel_samples],
                channels,
            )?;
            written += channel_samples;
            self.source_cursor += multiplier;
            self.consume_finished_chunks(ring, playhead);
        }

        self.commit_varispeed_position(ring, playhead);
        Ok(ReadProgress {
            samples: written,
            last_output_meta,
        })
    }

    pub(super) fn read_planar<'a>(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        pitch_bend: f32,
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

        let result = self.read(
            ring,
            events,
            playhead,
            recv,
            pitch_bend,
            &mut interleaved[..],
        );
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
        self.source_cursor = consumed_total.to_f64().unwrap_or(f64::INFINITY);
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

struct ReadProgress {
    samples: usize,
    last_output_meta: Option<PcmMeta>,
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

fn interpolated_fractional_position(
    meta: PcmMeta,
    consumed_frames: f64,
) -> kithara_platform::time::Duration {
    let total_frames = f64::from(meta.frames.max(1));
    let ratio = (consumed_frames / total_frames).clamp(0.0, 1.0);
    let start_ns = u64::try_from(meta.timestamp.as_nanos()).unwrap_or(u64::MAX);
    let span_ns = chunk_span_ns(meta);
    let position = start_ns.to_f64().unwrap_or(f64::INFINITY)
        + span_ns.to_f64().unwrap_or(f64::INFINITY) * ratio;
    let nanos = position.round().to_u64().unwrap_or(u64::MAX);
    kithara_platform::time::Duration::from_nanos(nanos)
}

fn chunk_span_ns(meta: PcmMeta) -> u64 {
    let start_ns = u64::try_from(meta.timestamp.as_nanos()).unwrap_or(u64::MAX);
    let end_ns = u64::try_from(meta.end_timestamp.as_nanos()).unwrap_or(u64::MAX);
    end_ns.saturating_sub(start_ns)
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
    use num_traits::cast::ToPrimitive;

    use super::*;
    use crate::audio::{Fetch, Inlet, Outlet, ThreadWake, connect, ring::RingParts};

    struct ReadFixture {
        ring: RingConsumer,
        cursor: ChunkCursor,
        events: AudioEvents,
        playhead: Arc<PlayheadState>,
        data_tx: Outlet<Fetch<PcmChunk>>,
        _trash_rx: Inlet<PcmChunk>,
    }

    impl ReadFixture {
        fn new(spec: PcmSpec) -> Self {
            let (data_tx, data_rx) = connect::<Fetch<PcmChunk>>(4, None);
            let (trash_tx, trash_rx) = connect::<PcmChunk>(8, None);
            let mut ring = RingConsumer::new(RingParts {
                pcm_rx: data_rx,
                trash_tx,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch: Arc::new(AtomicU64::new(0)),
                block_on_underrun: false,
            });
            ring.preloaded = true;
            Self {
                ring,
                cursor: ChunkCursor::new(&PcmPool::default().clone(), spec),
                events: AudioEvents::test(),
                playhead: Arc::new(PlayheadState::new()),
                data_tx,
                _trash_rx: trash_rx,
            }
        }

        fn push(&mut self, chunk: PcmChunk) {
            self.data_tx
                .try_push(Fetch::data(chunk, 0))
                .expect("chunk reaches test ring");
        }

        fn read(&mut self, pitch_bend: f32, samples: usize) -> (Vec<f32>, Duration) {
            let mut output = vec![0.0; samples];
            let read = self
                .cursor
                .read(
                    &mut self.ring,
                    &mut self.events,
                    self.playhead.as_ref(),
                    empty_recv(),
                    pitch_bend,
                    &mut output,
                )
                .expect("read succeeds");
            let (count, position) = match read.outcome {
                ReadOutcome::Frames { count, position } => (count, position),
                other => panic!("expected frames, got {other:?}"),
            };
            output.truncate(count.get());
            (output, position)
        }
    }

    fn empty_recv() -> RecvCtx<'static> {
        RecvCtx {
            cancel: None,
            worker: None,
            abr: None,
        }
    }

    fn sample_chunk(
        samples: &[f32],
        channels: u16,
        sample_rate: u32,
        frame_offset: u64,
        start_ns: u64,
        end_ns: u64,
    ) -> PcmChunk {
        let channels_usize = usize::from(channels).max(1);
        let frames = samples
            .len()
            .checked_div(channels_usize)
            .expect("test samples are frame-aligned");
        let spec = PcmSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
        );
        PcmChunk::new(
            PcmMeta {
                timestamp: Duration::from_nanos(start_ns),
                end_timestamp: Duration::from_nanos(end_ns),
                spec,
                frames: u32::try_from(frames).expect("test frame count fits u32"),
                frame_offset,
                ..PcmMeta::default()
            },
            PcmPool::default().attach(samples.to_vec()),
        )
    }

    fn assert_duration_near(actual: Duration, expected_ns: u64, tolerance_ns: u64) {
        let actual_ns = u64::try_from(actual.as_nanos()).expect("test duration fits u64");
        assert!(
            actual_ns.abs_diff(expected_ns) <= tolerance_ns,
            "duration {actual_ns}ns not within {tolerance_ns}ns of {expected_ns}ns"
        );
    }

    fn linear_at(samples: &[f32], position: f64) -> f32 {
        let base = position
            .floor()
            .to_usize()
            .expect("test source position fits usize");
        let fraction = (position - base.to_f64().expect("test base fits f64"))
            .to_f32()
            .expect("test fraction fits f32");
        let current = samples[base];
        let next = samples[base + 1];
        current + (next - current) * fraction
    }

    #[kithara::test]
    fn unity_is_bit_exact_for_interleaved_and_planar_reads() {
        let samples = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 5.5];
        let spec = PcmSpec::new(
            2,
            NonZeroU32::new(1_000).expect("test sample rate is non-zero"),
        );

        let mut fixture = ReadFixture::new(spec);
        fixture.push(sample_chunk(&samples[..6], 2, 1_000, 0, 0, 3_000_000));
        fixture.push(sample_chunk(
            &samples[6..],
            2,
            1_000,
            3,
            3_000_000,
            6_000_000,
        ));
        let (interleaved, _) = fixture.read(1.0, samples.len());
        assert_eq!(interleaved, samples);

        let mut planar = ReadFixture::new(spec);
        planar.push(sample_chunk(&samples[..6], 2, 1_000, 0, 0, 3_000_000));
        planar.push(sample_chunk(
            &samples[6..],
            2,
            1_000,
            3,
            3_000_000,
            6_000_000,
        ));
        let mut left = vec![0.0; 6];
        let mut right = vec![0.0; 6];
        let mut output: [&mut [f32]; 2] = [&mut left, &mut right];
        let read = planar
            .cursor
            .read_planar(
                &mut planar.ring,
                &mut planar.events,
                planar.playhead.as_ref(),
                empty_recv(),
                1.0,
                &mut output,
            )
            .expect("planar read succeeds");
        let count = match read.outcome {
            ReadOutcome::Frames { count, .. } => count,
            other => panic!("expected planar frames, got {other:?}"),
        };
        assert_eq!(count.get(), 6);
        assert_eq!(left, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(right, [0.5, 1.5, 2.5, 3.5, 4.5, 5.5]);
    }

    #[kithara::test]
    fn bend_scales_source_cursor_and_playhead_slope() {
        let source: Vec<f32> = (0..256)
            .map(|frame| frame.to_f32().expect("test frame fits f32"))
            .collect();
        let chunk = sample_chunk(&source, 1, 1_000, 0, 0, 256_000_000);
        let mut fixture = ReadFixture::new(chunk.spec());
        fixture.push(chunk);

        let (_, position) = fixture.read(1.02, 100);

        assert!((fixture.cursor.source_cursor - 102.0).abs() < 0.001);
        assert_duration_near(position, 102_000_000, 2_000);
    }

    #[kithara::test]
    fn bend_step_applies_to_next_read() {
        let source: Vec<f32> = (0..256)
            .map(|frame| frame.to_f32().expect("test frame fits f32"))
            .collect();
        let chunk = sample_chunk(&source, 1, 1_000, 0, 0, 256_000_000);
        let mut fixture = ReadFixture::new(chunk.spec());
        fixture.push(chunk);

        let (_, before_step) = fixture.read(1.0, 10);
        let (_, after_step) = fixture.read(2.0, 10);

        assert_duration_near(before_step, 10_000_000, 0);
        assert_duration_near(after_step, 30_000_000, 0);
    }

    #[kithara::test]
    fn varispeed_interpolates_across_chunk_seams() {
        let source: Vec<f32> = (0..96)
            .map(|frame| {
                let phase = frame.to_f32().expect("test frame fits f32") * 0.071;
                phase.sin()
            })
            .collect();
        let first = sample_chunk(&source[..32], 1, 1_000, 0, 0, 32_000_000);
        let second = sample_chunk(&source[32..], 1, 1_000, 32, 32_000_000, 96_000_000);
        let mut fixture = ReadFixture::new(first.spec());
        fixture.push(first);
        fixture.push(second);

        let (output, _) = fixture.read(1.1, 50);
        let multiplier = f64::from(1.1_f32);
        for (index, actual) in output.iter().copied().enumerate() {
            let expected = linear_at(
                &source,
                index.to_f64().expect("test index fits f64") * multiplier,
            );
            assert!(
                (actual - expected).abs() <= 0.000_001,
                "sample {index}: actual={actual} expected={expected}"
            );
        }
    }

    #[kithara::test]
    fn varispeed_carries_residual_across_chunk_seam() {
        let first = sample_chunk(&[0.0, 1.0, 2.0, 3.0], 1, 1_000, 0, 0, 4_000_000);
        let second = sample_chunk(&[10.0, 11.0, 12.0, 13.0], 1, 1_000, 4, 4_000_000, 8_000_000);
        let mut fixture = ReadFixture::new(first.spec());
        fixture.cursor.begin_chunk(&first);
        fixture.ring.current_chunk = Some(first);
        fixture.ring.lookahead = Some(second);
        fixture.cursor.source_cursor = 4.25;
        fixture.cursor.current_chunk_consumed_frames = 4;

        let mut output = [0.0; 1];
        let progress = fixture
            .cursor
            .read_varispeed(
                &mut fixture.ring,
                &mut fixture.events,
                fixture.playhead.as_ref(),
                empty_recv(),
                &mut output,
                1.0,
            )
            .expect("varispeed read succeeds");

        assert_eq!(progress.samples, 1);
        assert!((output[0] - 10.25).abs() <= f32::EPSILON);
        assert!((fixture.cursor.source_cursor - 1.25).abs() <= f64::EPSILON);
    }

    #[kithara::test]
    fn overall_audible_rate_is_capped() {
        let source: Vec<f32> = (0..1_000)
            .map(|frame| frame.to_f32().expect("test frame fits f32"))
            .collect();
        let chunk = sample_chunk(&source, 1, 1_000, 0, 0, 2_000_000_000);
        let mut fixture = ReadFixture::new(chunk.spec());
        fixture.push(chunk);

        let (_, position) = fixture.read(20.0, 10);

        assert_duration_near(position, 200_000_000, 1_000);
    }

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
                1.0,
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
                1.0,
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
