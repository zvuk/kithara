use std::{
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
    ops::ControlFlow,
};

use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{Activity, ActivityWriter, PlayheadRead, PlayheadWrite};
use kithara_test_utils::kithara;

use super::{
    super::{
        chunk_position,
        cursor::{ChunkCursor, ReadBuffer, source_spans_coalesce},
        normalize_geometry,
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
    seek_lock: Option<kithara_abr::AbrHandle>,
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
            seek_lock: None,
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

    delegate::delegate! {
        to self.source {
            /// Original raw coordinate of track-media zero.
            #[must_use]
            pub fn media_origin(&self) -> Option<Duration>;
            /// Logical media endpoint owned by the current source generation.
            #[must_use]
            pub fn media_end(&self) -> Option<Duration>;
            /// Original raw coordinate of the first visible PCM frame.
            #[must_use]
            pub fn visible_origin(&self) -> Option<Duration>;
        }
        to self.playhead {
            /// Current committed source position.
            #[must_use]
            pub fn position(&self) -> Duration;
            /// Total content duration, when known.
            #[must_use]
            pub fn duration(&self) -> Option<Duration>;
            /// Read-only source playback and cache metadata.
            #[must_use]
            #[call(clone)]
            pub fn playhead_read(&self) -> Arc<dyn PlayheadRead>;
        }
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
}

impl<S> Audio<S> {
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
    /// Seek inside the open source, discarding locally buffered PCM.
    /// Pending input is continued by deferred preparation on the owning lane.
    ///
    /// # Errors
    /// Returns a source or decoder failure without rebuilding for a seek failure.
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek_selected(position, false)
    }

    fn seek_selected(
        &mut self,
        position: Duration,
        for_render: bool,
    ) -> Result<SeekOutcome, AudioReadError> {
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
        let result = if for_render {
            self.source.seek_for_render(position)
        } else {
            self.source.seek(position)
        };
        if let Err(error) = &result {
            self.failure = Some(crate::TrackFailureKind::from(error));
        }
        if !matches!(result, Ok(SeekOutcome::Pending { .. }))
            && let Some(spec) = self.source.prepare_deferred()
        {
            self.cursor.set_spec(spec);
        }
        self.source.finish_deferred();
        result
    }
    fn seek_controlled(
        &mut self,
        position: Duration,
        for_render: bool,
    ) -> Result<SeekOutcome, AudioReadError> {
        if self.seek_lock.is_none()
            && let Some(abr) = &self.abr
        {
            abr.lock();
            self.seek_lock = Some(abr.clone());
        }
        let result = self.seek_selected(position, for_render);
        self.seek_pending = matches!(
            result,
            Ok(SeekOutcome::Landed { .. } | SeekOutcome::Pending { .. })
        );
        if !self.seek_pending {
            self.release_seek_lock();
        }
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
            TrackStep::Produced(Fetch::NaturalEof) | TrackStep::Eof => {
                self.seek_pending = false;
                self.release_seek_lock();
                self.publish_final_progress();
                self.emit.flush();
                Ok(ChunkOutcome::Eof {
                    position: self.position(),
                })
            }
            TrackStep::Produced(Fetch::Failure { failure }) | TrackStep::Failed(failure) => {
                self.failure = Some(failure);
                self.release_seek_lock();
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
            if chunk.meta.frames > 0 {
                self.commit_read(chunk.meta);
            }
            self.publish_progress();
            self.emit.flush();
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
    #[kithara::hang_watchdog]
    fn read_into(&mut self, mut output: ReadBuffer<'_, '_>) -> Result<ReadOutcome, AudioReadError> {
        let capacity = output.capacity()?;
        let mut written = 0usize;
        let mut source_span: Option<SourceSpan> = None;
        let mut source_meta: Option<AudioChunkInfo> = None;
        let mut output_frames = 0u64;
        let mut eof = false;
        while written < capacity {
            hang_tick!();
            match self.ensure_read_chunk() {
                Err(_) if written > 0 => break,
                Err(error) => return Err(error),
                Ok(ControlFlow::Break(outcome)) => {
                    eof = matches!(outcome, ChunkOutcome::Eof { .. });
                    break;
                }
                Ok(ControlFlow::Continue(())) => {}
            }
            let Some(chunk) = self.current_chunk.as_mut() else {
                break;
            };
            let span = Some(normalize_geometry(&mut chunk.meta)?);
            if written > 0
                && !read_geometry_coalesces(
                    source_meta,
                    &chunk.meta,
                    source_span,
                    output_frames,
                    span,
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
            let meta = chunk.meta;
            written += copied.count;
            if copied.count > 0 {
                source_meta = Some(meta);
                self.commit_read(meta);
                hang_reset!();
            }
            output_frames = output_frames.saturating_add(copied.output_frames);
            source_span = join_read_spans(source_span, copied.source_span);
            if copied.finished {
                self.current_chunk = None;
            } else if copied.count == 0 {
                break;
            }
        }
        Ok(self.finish_read(written, source_span, eof))
    }

    fn ensure_read_chunk(&mut self) -> Result<ControlFlow<ChunkOutcome>, AudioReadError> {
        if self.current_chunk.is_some() {
            return Ok(ControlFlow::Continue(()));
        }
        match self.pull_chunk()? {
            ChunkOutcome::Chunk(chunk) => {
                self.cursor.begin_chunk(&chunk);
                self.current_chunk = Some(*chunk);
                Ok(ControlFlow::Continue(()))
            }
            outcome => Ok(ControlFlow::Break(outcome)),
        }
    }

    fn finish_read(
        &mut self,
        written: usize,
        source_span: Option<SourceSpan>,
        eof: bool,
    ) -> ReadOutcome {
        if let Some(count) = NonZeroUsize::new(written) {
            self.release_seek_lock();
            self.publish_progress();
            self.emit.flush();
            return ReadOutcome::Frames {
                count,
                position: self.position(),
                source_span,
            };
        }
        if eof {
            ReadOutcome::Eof {
                position: self.position(),
            }
        } else {
            ReadOutcome::Pending {
                reason: PendingReason::StreamBackpressure,
                position: self.position(),
            }
        }
    }

    fn release_seek_lock(&mut self) {
        if let Some(abr) = self.seek_lock.take() {
            abr.unlock();
        }
    }

    fn commit_read(&mut self, meta: AudioChunkInfo) {
        self.release_seek_lock();
        if std::mem::take(&mut self.seek_pending) {
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
    }

    fn publish_progress(&mut self) {
        let position_ms = self.position_ms();
        if self.last_progress_emit.is_some_and(|last| {
            position_ms.abs_diff(last) < crate::consts::PROGRESS_EMIT_MIN_DELTA_MS
        }) {
            return;
        }
        self.emit_progress(position_ms);
    }

    /// The end of the track is the throttle's trailing edge: observers always
    /// see the final committed position.
    fn publish_final_progress(&mut self) {
        let position_ms = self.position_ms();
        if self.last_progress_emit != Some(position_ms) {
            self.emit_progress(position_ms);
        }
    }

    fn position_ms(&self) -> u64 {
        u64::try_from(self.position().as_millis()).unwrap_or(u64::MAX)
    }

    fn emit_progress(&mut self, position_ms: u64) {
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
}

fn read_geometry_coalesces(
    previous_meta: Option<AudioChunkInfo>,
    current_meta: &AudioChunkInfo,
    previous_span: Option<SourceSpan>,
    output_frames: u64,
    current_span: Option<SourceSpan>,
) -> bool {
    let changed = previous_meta.is_some_and(|previous| {
        previous.source_revision != current_meta.source_revision
            || previous.visible_origin != current_meta.visible_origin
            || previous.media_origin != current_meta.media_origin
    });
    !changed
        && source_spans_coalesce(
            previous_span,
            output_frames,
            current_span,
            u64::from(current_meta.frames),
        )
}

fn join_read_spans(previous: Option<SourceSpan>, next: Option<SourceSpan>) -> Option<SourceSpan> {
    match (previous, next) {
        (Some(previous), Some(next)) => previous.followed_by(next),
        (None, span) => span,
        _ => None,
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
        self.release_seek_lock();
    }
}
impl<S> AudioRead for Audio<S> {
    fn cached_span(&self) -> Duration {
        self.playhead.cached()
    }

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
            fn media_origin(&self) -> Option<Duration>;
            fn visible_origin(&self) -> Option<Duration>;
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
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
        }
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek_controlled(position, false)
    }
}
impl<S: 'static> AudioSource for Audio<S> {
    type Chunk = AudioChunk;

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        AudioControl::seek(self, position)
    }

    fn seek_for_render(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        self.seek_controlled(position, true)
    }

    fn finish_segment(&mut self) {
        self.source.finish_segment();
        self.publish_final_progress();
    }

    fn commit_source_end(&mut self, end: SourceEnd, meta: AudioChunkInfo) {
        self.source.commit_source_end(end, meta);
        if meta.frames == 0 {
            return;
        }
        self.playhead.advance(&chunk_position(&meta));
        self.commit_read(meta);
        self.publish_progress();
    }

    delegate::delegate! {
        to self {
            fn set_host_sample_rate(&mut self, rate: NonZeroU32);
            fn media_origin(&self) -> Option<Duration>;
            fn media_end(&self) -> Option<Duration>;
            fn visible_origin(&self) -> Option<Duration>;
        }
        to self.source {
            fn native_pcm_basis(&self) -> Option<AudioChunkInfo>;
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
                    self.release_seek_lock();
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
                        self.release_seek_lock();
                        return TrackStep::Failed(failure);
                    }
                };
                self.cursor.begin_chunk(&chunk);
                return TrackStep::Produced(Fetch::rendered(chunk, end));
            }
        }
        let step = self.source.step_track();
        match step {
            TrackStep::Produced(Fetch::NaturalEof) | TrackStep::Eof => {
                self.release_seek_lock();
                self.publish_final_progress();
            }
            TrackStep::Produced(Fetch::Failure { .. }) | TrackStep::Failed(_) => {
                self.release_seek_lock();
            }
            TrackStep::Produced(Fetch::Data { .. })
            | TrackStep::Blocked(_)
            | TrackStep::StateChanged => {}
        }
        step
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::tokio::task::yield_runnable;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::mock::{prepared_audio, produced_audio};

    #[kithara::test(native, tokio)]
    #[kithara::hang_watchdog]
    async fn lane_seek_events_publish_once_in_commit_order_without_open_seek_completion() {
        use crate::{AudioEvent, AudioSource, SeekLifecycleStage, SeekOutcome};

        let mut audio = prepared_audio().await;
        audio.warm_up();
        let mut events = audio.events::<AudioEvent>();
        audio.seek(Duration::ZERO).expect("open-time positioning");
        let chunk = produced_audio(&mut audio).await;
        let end = source_end(&chunk.meta, u64::from(chunk.meta.frames)).expect("source boundary");
        audio.commit_source_end(end, chunk.meta);
        audio.finish_deferred();
        while let Ok(envelope) = events.try_recv() {
            hang_tick!();
            hang_reset!();
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
                Ok(SeekOutcome::Landed { target: accepted, .. }
                    | SeekOutcome::Pending { target: accepted, .. }) if accepted == target
            ));
            let chunk = produced_audio(&mut audio).await;
            audio.finish_deferred();
            let mut stages = Vec::new();
            while let Ok(envelope) = events.try_recv() {
                hang_tick!();
                hang_reset!();
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
                hang_tick!();
                hang_reset!();
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
        let chunk = produced_audio(&mut audio).await;
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

    #[derive(Clone, Copy, Debug)]
    enum Consumer {
        Lane,
        Chunks,
    }

    #[kithara::test(native, tokio)]
    #[case::render_lane(Consumer::Lane)]
    #[case::chunk_reader(Consumer::Chunks)]
    async fn the_end_of_track_publishes_the_final_committed_position(#[case] consumer: Consumer) {
        use crate::{AudioEvent, AudioSource, SeekOutcome};

        let mut audio = prepared_audio().await;
        audio.warm_up();
        let first = produced_audio(&mut audio).await;
        audio.finish_deferred();
        let duration = audio.duration().expect("WAV duration");
        let chunk = first.meta.end_timestamp - first.meta.timestamp;
        // WHY: one and a half chunks before the end puts the last two
        // commits inside one throttle window.
        let target = duration - chunk * 3 / 2;
        assert!(matches!(
            AudioSource::seek(&mut audio, target),
            Ok(SeekOutcome::Landed { target: accepted, .. }
                | SeekOutcome::Pending { target: accepted, .. }) if accepted == target
        ));
        let mut events = audio.events::<AudioEvent>();
        drain_to_end(&mut audio, consumer).await;
        audio.finish_deferred();
        let mut published = Vec::new();
        while let Ok(envelope) = events.try_recv() {
            if let AudioEvent::PlaybackProgress { position_ms, .. } = envelope.event {
                published.push(position_ms);
            }
        }
        let end_ms = u64::try_from(audio.position().as_millis()).expect("position in millis");
        assert_eq!(
            published.last(),
            Some(&end_ms),
            "the throttled tail reaches observers: {published:?}"
        );
    }

    #[kithara::hang_watchdog]
    #[kithara::flash(io)]
    async fn drain_to_end<S: 'static>(audio: &mut Audio<S>, consumer: Consumer) {
        use crate::{AudioSource, Fetch, TrackStep};

        loop {
            hang_tick!();
            match consumer {
                Consumer::Lane => {
                    let _ = audio.prepare_deferred();
                    match audio.step_track() {
                        TrackStep::Produced(Fetch::Data { data, .. }) => {
                            let end = source_end(&data.meta, u64::from(data.meta.frames))
                                .expect("source boundary");
                            audio.commit_source_end(end, data.meta);
                            hang_reset!();
                        }
                        TrackStep::Produced(Fetch::NaturalEof) | TrackStep::Eof => return,
                        TrackStep::StateChanged | TrackStep::Blocked(_) => {}
                        _ => panic!("the WAV fixture plays to its end"),
                    }
                    audio.finish_deferred();
                }
                Consumer::Chunks => match audio.next_chunk().expect("chunk read") {
                    ChunkOutcome::Chunk(_) => {
                        hang_reset!();
                    }
                    ChunkOutcome::Eof { .. } => return,
                    ChunkOutcome::Pending { .. } => {}
                },
            }
            yield_runnable().await;
        }
    }
}
