use std::{
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{Activity, ActivityWriter, PlayheadWrite};

use super::{
    super::{
        chunk_position,
        cursor::{ChunkCursor, ReadBuffer, source_spans_coalesce},
    },
    context::AudioContext,
};
use crate::{
    AudioControl, AudioRead, AudioReadError, AudioSession, AudioSource, ChunkOutcome,
    FailureSource, Fetch, PendingReason, ReadOutcome, SeekOutcome, SourceEnd, SourceSpan,
    TrackStep,
};

/// Open decoded source owned and driven by one lane thread.
pub struct Audio<S> {
    source: Box<dyn AudioSource<Chunk = AudioChunk>>,
    playhead: Arc<dyn PlayheadWrite>,
    emit: Arc<DeferredBus<crate::AudioLaneEvent>>,
    seek_pending: bool,
    last_progress_emit: Option<u64>,
    metadata: TrackMetadata,
    abr: Option<kithara_abr::AbrHandle>,
    activity: Activity,
    activity_writer: Option<ActivityWriter>,
    cancel: CancelToken,
    pub(super) cursor: ChunkCursor,
    pub(super) current_chunk: Option<AudioChunk>,
    preloaded: bool,
    failure: Option<crate::TrackFailureKind>,
    marker: PhantomData<fn() -> S>,
}

impl<S> Audio<S> {
    pub(in crate::audio) fn new(
        source: Box<dyn AudioSource<Chunk = AudioChunk>>,
        context: AudioContext,
        spec: AudioSpec,
    ) -> Self {
        Self {
            source,
            playhead: context.playhead,
            emit: context.emit,
            seek_pending: false,
            last_progress_emit: None,
            metadata: context.metadata,
            abr: context.abr,
            activity: context.activity,
            activity_writer: context.activity_writer,
            cancel: context.cancel,
            cursor: ChunkCursor::new(spec),
            current_chunk: None,
            preloaded: false,
            failure: None,
            marker: PhantomData,
        }
    }
    /// Transfer the sole loader-priority writer to the owning lane.
    pub fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.activity_writer.take()
    }
    /// Read-only loader-priority snapshot.
    #[must_use]
    pub fn activity(&self) -> Activity {
        self.activity.clone()
    }
    /// Adaptive bitrate control for this open source.
    #[must_use]
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.abr.clone()
    }
    /// Currently selected adaptive variant.
    #[must_use]
    pub fn current_variant(&self) -> Option<kithara_abr::VariantInfo> {
        self.abr.as_ref()?.current_variant()
    }
    /// Whether initial input preparation was requested.
    #[must_use]
    pub const fn is_preloaded(&self) -> bool {
        self.preloaded
    }
    /// Track metadata captured at open.
    #[must_use]
    pub const fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
    /// Current decoded output format.
    #[must_use]
    pub fn spec(&self) -> AudioSpec {
        self.cursor.spec()
    }

    delegate::delegate! {
        to self.playhead {
            /// Current committed source position.
            #[must_use]
            pub fn position(&self) -> Duration;
            /// Total content duration, when known.
            #[must_use]
            pub fn duration(&self) -> Option<Duration>;
        }
    }

    /// Prepare one initial chunk without moving work to another thread.
    ///
    /// # Errors
    /// Returns a source or decoder failure.
    pub fn preload(&mut self) -> Result<(), AudioReadError> {
        self.preloaded = true;
        if self.current_chunk.is_none()
            && let ChunkOutcome::Chunk(chunk) = self.pull_chunk()?
        {
            self.cursor.begin_chunk(&chunk);
            self.current_chunk = Some(*chunk);
        }
        Ok(())
    }
    /// Seek synchronously inside the open source, discarding locally buffered PCM.
    ///
    /// # Errors
    /// Returns a source or decoder failure without rebuilding for a seek failure.
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek_pending = false;
        self.last_progress_emit = None;
        if let Some(failure) = self.failure {
            return Err(AudioReadError::Stream {
                what: "seek decoded source",
                source: FailureSource::ProducerAfterSeek { failure },
            });
        }
        self.current_chunk = None;
        self.cursor.clear();
        let result = self.source.seek(position);
        if let Err(error) = &result {
            self.failure = Some(crate::TrackFailureKind::from(error));
        }
        if let Some(spec) = self.source.prepare_deferred() {
            self.cursor.set_spec(spec);
        }
        self.source.finish_deferred();
        result
    }
    /// Rebuild the decoder at a host-rate change on the owning thread.
    /// A rebuild error becomes the source's terminal read failure.
    pub fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        if self.source.host_sample_rate() == Some(rate) {
            return;
        }
        self.current_chunk = None;
        self.cursor.clear();
        self.source.set_host_sample_rate(rate);
        if let Some(spec) = self.source.prepare_deferred() {
            self.cursor.set_spec(spec);
        }
        self.source.finish_deferred();
    }
    fn pull_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        if let Some(failure) = self.failure {
            return Err(AudioReadError::Stream {
                what: "read decoded source",
                source: FailureSource::Producer { failure },
            });
        }
        if let Some(spec) = self.source.prepare_deferred() {
            self.cursor.set_spec(spec);
        }
        let step = self.source.step_track();
        self.source.finish_deferred();
        match step {
            TrackStep::Produced(Fetch::Data { data, .. }) => {
                Ok(ChunkOutcome::Chunk(Box::new(data)))
            }
            TrackStep::Produced(Fetch::NaturalEof) | TrackStep::Eof => Ok(ChunkOutcome::Eof {
                position: self.position(),
            }),
            TrackStep::Produced(Fetch::Failure { failure }) | TrackStep::Failed(failure) => {
                self.failure = Some(failure);
                Err(AudioReadError::Stream {
                    what: "read decoded source",
                    source: FailureSource::Producer { failure },
                })
            }
            TrackStep::Blocked(_) | TrackStep::StateChanged => Ok(ChunkOutcome::Pending {
                reason: PendingReason::StreamBackpressure,
                position: self.position(),
            }),
        }
    }
    /// Pull a chunk on the owning thread.
    ///
    /// # Errors
    /// Returns a source or decoder failure.
    pub fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        let chunk = if let Some(chunk) = self.current_chunk.take() {
            let mut consumed = self.cursor.consumed_frames();
            crate::pipeline::seek::skip::apply_frames(chunk, &mut consumed)?
        } else {
            None
        };
        let outcome = match chunk {
            Some(chunk) => ChunkOutcome::Chunk(Box::new(chunk)),
            None => self.pull_chunk()?,
        };
        if let ChunkOutcome::Chunk(chunk) = &outcome {
            self.cursor.begin_chunk(chunk);
            self.playhead.advance(&chunk_position(&chunk.meta));
            self.source.commit_source_end(
                source_end(&chunk.meta, u64::from(chunk.meta.frames))?,
                chunk.meta,
            );
        }
        Ok(outcome)
    }
    /// Copy interleaved samples from the open source.
    ///
    /// # Errors
    /// Returns invalid buffer geometry or a source failure.
    pub fn read(&mut self, output: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
        self.read_into(ReadBuffer::Interleaved(output))
    }
    /// Copy samples into equal-length channel planes.
    ///
    /// # Errors
    /// Returns invalid plane geometry or a source failure.
    pub fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        self.read_into(ReadBuffer::Planar(output))
    }
    fn read_into(&mut self, mut output: ReadBuffer<'_, '_>) -> Result<ReadOutcome, AudioReadError> {
        let capacity = output.capacity()?;
        let mut written = 0usize;
        let mut source_span: Option<SourceSpan> = None;
        let mut output_frames = 0u64;
        let mut eof = false;
        while written < capacity {
            if self.current_chunk.is_none() {
                match self.pull_chunk() {
                    Err(_) if written > 0 => break,
                    Err(error) => return Err(error),
                    Ok(outcome) => match outcome {
                        ChunkOutcome::Chunk(chunk) => {
                            self.cursor.begin_chunk(&chunk);
                            self.current_chunk = Some(*chunk);
                        }
                        ChunkOutcome::Eof { .. } => {
                            eof = true;
                            break;
                        }
                        ChunkOutcome::Pending { .. } => break,
                    },
                }
            }
            let Some(chunk) = self.current_chunk.as_ref() else {
                break;
            };
            let span = chunk.meta.source_span.map_or_else(
                || {
                    SourceSpan::new(
                        chunk.meta.frame_offset,
                        chunk
                            .meta
                            .frame_offset
                            .saturating_add(u64::from(chunk.meta.frames)),
                        chunk.spec().sample_rate,
                        u64::from(chunk.meta.frames),
                    )
                    .map(|span| {
                        span.with_render_revision(chunk.meta.render_revision)
                            .with_mapping_revision(chunk.meta.mapping_revision)
                    })
                },
                Some,
            );
            if written > 0
                && !source_spans_coalesce(
                    source_span,
                    output_frames,
                    span,
                    u64::from(chunk.meta.frames),
                )
            {
                break;
            }
            let copied =
                self.cursor
                    .copy_into(chunk, span, &mut output, written, self.playhead.as_ref())?;
            self.source.commit_source_end(
                source_end(&chunk.meta, self.cursor.consumed_frames())?,
                chunk.meta,
            );
            written += copied.count;
            output_frames = output_frames.saturating_add(copied.output_frames);
            source_span = match (source_span, copied.source_span) {
                (Some(previous), Some(next)) => previous.followed_by(next),
                (None, span) => span,
                _ => None,
            };
            if copied.finished {
                self.current_chunk = None;
            } else if copied.count == 0 {
                break;
            }
        }
        if let Some(count) = NonZeroUsize::new(written) {
            return Ok(ReadOutcome::Frames {
                count,
                position: self.position(),
                source_span,
            });
        }
        if eof {
            Ok(ReadOutcome::Eof {
                position: self.position(),
            })
        } else {
            Ok(ReadOutcome::Pending {
                reason: PendingReason::StreamBackpressure,
                position: self.position(),
            })
        }
    }
}

pub(super) fn source_end(meta: &AudioChunkInfo, consumed: u64) -> Result<SourceEnd, DecodeError> {
    if let Some(span) = meta.source_span {
        let span = span
            .for_output_range(0..consumed)
            .ok_or(DecodeError::InvalidData {
                detail: "rendered source mapping does not cover PCM",
            })?;
        return Ok(SourceEnd::new(span.end(), span.sample_rate())
            .with_mapping_revision(span.mapping_revision()));
    }
    Ok(SourceEnd::new(
        meta.frame_offset.saturating_add(consumed),
        meta.spec.sample_rate,
    )
    .with_mapping_revision(meta.mapping_revision))
}

impl<S> Drop for Audio<S> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl<S> AudioRead for Audio<S> {
    fn decoded_frontier(&self) -> Duration {
        self.current_chunk
            .as_ref()
            .map_or_else(|| self.position(), |chunk| chunk.meta.end_timestamp)
    }

    delegate::delegate! {
        to self {
            fn spec(&self) -> AudioSpec;
            fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError>;
            fn position(&self) -> Duration;
            fn read(&mut self, output: &mut [f32]) -> Result<ReadOutcome, AudioReadError>;
            fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<ReadOutcome, AudioReadError>;
        }
    }
}
impl<S> AudioSession for Audio<S> {
    delegate::delegate! {
        to self {
            fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn duration(&self) -> Option<Duration>;
            fn is_preloaded(&self) -> bool;
            fn metadata(&self) -> &TrackMetadata;
        }
    }

    fn event_bus(&self) -> &EventBus {
        self.emit.bus()
    }
}
impl<S> AudioControl for Audio<S> {
    delegate::delegate! {
        to self {
            fn preload(&mut self) -> Result<(), AudioReadError>;
            fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
        }
    }
}
impl<S: 'static> AudioSource for Audio<S> {
    type Chunk = AudioChunk;

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        let outcome = self.seek(position)?;
        self.seek_pending = matches!(outcome, SeekOutcome::Landed { .. });
        Ok(outcome)
    }

    fn commit_source_end(&mut self, end: SourceEnd, meta: AudioChunkInfo) {
        self.source.commit_source_end(end, meta);
        if meta.frames == 0 {
            return;
        }
        self.playhead.advance(&chunk_position(&meta));
        if self.seek_pending {
            self.seek_pending = false;
            self.emit.enqueue(crate::AudioEvent::SeekLifecycle {
                stage: crate::SeekLifecycleStage::OutputCommitted,
                location: crate::SegmentLocation::new(
                    meta.variant_index,
                    meta.segment_index,
                    None,
                    None,
                ),
            });
            self.emit.enqueue(crate::AudioEvent::SeekComplete {
                position: meta.timestamp,
            });
        }
        let position_ms = u64::try_from(self.position().as_millis()).unwrap_or(u64::MAX);
        if self.last_progress_emit.is_some_and(|last| {
            position_ms.abs_diff(last) < crate::consts::PROGRESS_EMIT_MIN_DELTA_MS
        }) {
            return;
        }
        self.last_progress_emit = Some(position_ms);
        let total_ms = self
            .duration()
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        let decoded_ms =
            u64::try_from(self.playhead.decoded_frontier().as_millis()).unwrap_or(u64::MAX);
        self.emit.enqueue(crate::AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            buffered_ms: Some(total_ms.map_or(decoded_ms, |total| decoded_ms.min(total))),
        });
    }

    delegate::delegate! {
        to self {
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
        }
        to self.source {
            fn host_sample_rate(&self) -> Option<NonZeroU32>;
            fn discontinuity(&self) -> Option<crate::SourceDiscontinuity>;
            fn finish_deferred(&mut self);
            fn prepare_deferred(&mut self) -> Option<AudioSpec>;
            fn warm_up(&mut self);
        }
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if let Some(failure) = self.failure {
            return TrackStep::Failed(failure);
        }
        if let Some(chunk) = self.current_chunk.take() {
            let mut consumed = self.cursor.consumed_frames();
            let chunk = match crate::pipeline::seek::skip::apply_frames(chunk, &mut consumed) {
                Ok(chunk) => chunk,
                Err(error) => {
                    let failure = crate::TrackFailureKind::Decode {
                        kind: crate::map_decode_error_kind(&error),
                    };
                    self.failure = Some(failure);
                    return TrackStep::Failed(failure);
                }
            };
            if let Some(chunk) = chunk {
                let end = match source_end(&chunk.meta, u64::from(chunk.meta.frames)) {
                    Ok(end) => end,
                    Err(error) => {
                        let failure = crate::TrackFailureKind::Decode {
                            kind: crate::map_decode_error_kind(&error),
                        };
                        self.failure = Some(failure);
                        return TrackStep::Failed(failure);
                    }
                };
                self.cursor.begin_chunk(&chunk);
                return TrackStep::Produced(Fetch::rendered(chunk, end));
            }
        }
        self.source.step_track()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::mock::{prepared_audio, produced_audio};

    #[kithara::test(native, tokio)]
    async fn lane_seek_events_publish_once_in_commit_order_without_open_seek_completion() {
        use crate::{AudioEvent, AudioSource, SeekLifecycleStage, SeekOutcome};

        let mut audio = prepared_audio().await;
        audio.warm_up();
        let mut events = audio.events::<AudioEvent>();
        audio.seek(Duration::ZERO).expect("open-time positioning");
        let chunk = produced_audio(&mut audio);
        let end = source_end(&chunk.meta, u64::from(chunk.meta.frames)).expect("source boundary");
        audio.commit_source_end(end, chunk.meta);
        audio.finish_deferred();
        while let Ok(envelope) = events.try_recv() {
            assert!(!matches!(envelope.event, AudioEvent::SeekComplete { .. }));
            assert!(!matches!(
                envelope.event,
                AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::OutputCommitted,
                    ..
                }
            ));
        }

        for target in [Duration::from_millis(20), Duration::from_millis(40)] {
            assert!(matches!(
                AudioSource::seek(&mut audio, target),
                Ok(SeekOutcome::Landed { .. })
            ));
            let chunk = produced_audio(&mut audio);
            audio.finish_deferred();
            let mut stages = Vec::new();
            while let Ok(envelope) = events.try_recv() {
                match envelope.event {
                    AudioEvent::SeekLifecycle { stage, .. } => stages.push(stage),
                    AudioEvent::SeekComplete { .. } => panic!("seek completed before admission"),
                    _ => {}
                }
            }
            assert_eq!(
                stages,
                [
                    SeekLifecycleStage::SeekRequest,
                    SeekLifecycleStage::SeekApplied
                ]
            );
            let end =
                source_end(&chunk.meta, u64::from(chunk.meta.frames)).expect("source boundary");
            audio.commit_source_end(end, chunk.meta);
            assert!(
                events.try_recv().is_err(),
                "commit is deferred until the shell flush"
            );
            audio.finish_deferred();
            assert!(matches!(events.try_recv().expect("output commit").event,
                AudioEvent::SeekLifecycle { stage: SeekLifecycleStage::OutputCommitted, location }
                    if location.variant == chunk.meta.variant_index
                        && location.segment_index == chunk.meta.segment_index));
            assert!(matches!(events.try_recv().expect("seek complete").event,
                AudioEvent::SeekComplete { position } if position == chunk.meta.timestamp));
            stages.push(SeekLifecycleStage::OutputCommitted);
            assert_eq!(
                stages,
                [
                    SeekLifecycleStage::SeekRequest,
                    SeekLifecycleStage::SeekApplied,
                    SeekLifecycleStage::OutputCommitted,
                ]
            );
            assert_eq!(audio.position(), chunk.meta.end_timestamp);
            audio.commit_source_end(end, chunk.meta);
            audio.finish_deferred();
            while let Ok(envelope) = events.try_recv() {
                assert!(!matches!(envelope.event, AudioEvent::SeekComplete { .. }));
                assert!(!matches!(
                    envelope.event,
                    AudioEvent::SeekLifecycle {
                        stage: SeekLifecycleStage::OutputCommitted,
                        ..
                    }
                ));
            }
        }
    }

    #[kithara::test(native, tokio)]
    async fn committed_progress_is_throttled_by_one_hundred_milliseconds() {
        use crate::{AudioEvent, AudioSource, SourceEnd};

        let mut audio = prepared_audio().await;
        audio.warm_up();
        let chunk = produced_audio(&mut audio);
        audio.finish_deferred();
        let mut events = audio.events::<AudioEvent>();
        audio
            .playhead
            .set_duration(Some(Duration::from_millis(1_000)));
        audio
            .playhead
            .set_decoded_frontier(Duration::from_millis(1_500));
        for millis in [10, 50, 110] {
            let mut meta = chunk.meta;
            meta.frames = 1;
            meta.end_timestamp = Duration::from_millis(millis);
            let frame = meta
                .spec
                .frame_at(meta.end_timestamp)
                .expect("committed frame");
            audio.commit_source_end(SourceEnd::new(frame, meta.spec.sample_rate), meta);
        }
        assert!(events.try_recv().is_err(), "progress is deferred");
        audio.finish_deferred();
        for expected in [10, 110] {
            assert!(matches!(events.try_recv().expect("progress").event,
                AudioEvent::PlaybackProgress { position_ms, total_ms: Some(1_000), buffered_ms: Some(1_000) }
                    if position_ms == expected));
        }
        assert!(
            events.try_recv().is_err(),
            "the middle commit stays throttled"
        );
    }
}
