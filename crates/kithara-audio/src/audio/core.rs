use std::{
    io::Error as IoError,
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmSpec, TrackMetadata, duration_for_frames};
use kithara_events::{AudioEvent, EventBus, SeekLifecycleStage, SegmentLocation};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_stream::{DeferredWake, PlayheadWrite, SeekControl, SeekIntent, SeekObserve};
use portable_atomic::AtomicF32;
use tracing::trace;

use super::{
    AtomicServiceClass, AudioWorkerHandle, ChunkOutcome, DecodeError, PcmControl, PcmRead,
    PcmSession, PendingReason, PreloadGate, ReadOutcome, SeekOutcome, ServiceClass,
    StretchControls, TrackId,
    cursor::ChunkCursor,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer},
};
use crate::{
    SourceRange, SourceRangeError, SourceRangeReadOutcome, SourceRangeRequest,
    effects::transport::PitchBend,
    source_range::{AtomicAudioReadMode, AudioReadMode},
};

/// Pull-based PCM facade backed by a shared renderer worker.
pub struct Audio<S> {
    lease: WorkerLease,
    ring: RingConsumer,
    cursor: ChunkCursor,
    events: AudioEvents,
    session: Session,
    controls: Controls,
    source_range: Option<SourceRangeCursor>,
    _marker: PhantomData<S>,
}

pub(super) struct AudioParts<S> {
    pub(super) lease: WorkerLease,
    pub(super) ring: RingConsumer,
    pub(super) session: Session,
    pub(super) controls: Controls,
    pub(super) pcm_pool: PcmPool,
    pub(super) spec: PcmSpec,
    pub(super) emit: Arc<kithara_events::DeferredBus<kithara_events::Event>>,
    pub(super) marker: PhantomData<S>,
}

pub(super) struct Session {
    pub(super) playhead: Arc<dyn PlayheadWrite>,
    pub(super) preload_gate: Arc<PreloadGate>,
    pub(super) seek: Arc<dyn SeekControl>,
    pub(super) seek_obs: Arc<dyn SeekObserve>,
    pub(super) metadata: TrackMetadata,
    pub(super) abr_handle: Option<kithara_abr::AbrHandle>,
    pub(super) peer_wake: Option<Arc<DeferredWake>>,
}

pub(super) struct Controls {
    pub(super) host_sample_rate: Arc<AtomicU32>,
    pub(super) playback_rate: Arc<AtomicF32>,
    pub(super) pitch_bend: PitchBend,
    pub(super) stretch: Option<Arc<StretchControls>>,
    pub(super) service_class: Arc<AtomicServiceClass>,
    pub(super) read_mode: Arc<AtomicAudioReadMode>,
}

#[derive(Clone, Copy)]
struct SourceRangeCursor {
    request: SourceRangeRequest,
    next_frame: u64,
}

pub(super) struct WorkerLease {
    pub(super) cancel: Option<CancelToken>,
    pub(super) track_id: Option<TrackId>,
    pub(super) worker: Option<AudioWorkerHandle>,
    pub(super) is_standalone: bool,
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        if let (Some(worker), Some(track_id)) = (&self.worker, self.track_id.take()) {
            worker.unregister_track(track_id);
            if self.is_standalone {
                worker.shutdown();
            }
        }
    }
}

impl<S> From<AudioParts<S>> for Audio<S> {
    fn from(parts: AudioParts<S>) -> Self {
        Self {
            lease: parts.lease,
            ring: parts.ring,
            cursor: ChunkCursor::new(&parts.pcm_pool, parts.spec),
            events: AudioEvents::new(parts.emit.bus().clone()),
            session: parts.session,
            controls: parts.controls,
            source_range: None,
            _marker: parts.marker,
        }
    }
}

impl<S> Audio<S> {
    #[must_use]
    /// Returns the adaptive-bitrate handle for adaptive sources.
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.session.abr_handle.clone()
    }

    #[must_use]
    /// Returns metadata for the currently selected adaptive variant.
    pub fn current_variant(&self) -> Option<kithara_events::VariantInfo> {
        self.session.abr_handle.as_ref()?.current_variant()
    }

    #[must_use]
    /// Returns the known stream duration.
    pub fn duration(&self) -> Option<Duration> {
        self.session.playhead.duration()
    }

    #[must_use]
    /// Reports whether non-blocking reads have been enabled.
    pub fn is_preloaded(&self) -> bool {
        self.ring.preloaded
    }

    #[must_use]
    /// Returns track metadata.
    pub fn metadata(&self) -> &TrackMetadata {
        &self.session.metadata
    }

    #[must_use]
    /// Returns the committed playback position.
    pub fn position(&self) -> Duration {
        self.session.playhead.position()
    }

    /// Enables non-blocking reads and primes the first PCM chunk.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if the producer channel closes during preload.
    pub fn preload(&mut self) -> Result<(), DecodeError> {
        if !self.is_preloaded() {
            self.ring.preloaded = true;
        }
        if self.ring.current_chunk.is_none() && self.ring.phase != super::ConsumerPhase::AtEof {
            self.fill_buffer();
            if self.ring.phase == super::ConsumerPhase::Failed {
                return Err(channel_closed_during_preload());
            }
        }
        Ok(())
    }

    pub(crate) fn fill_buffer(&mut self) -> bool {
        let recv = recv_ctx(&self.session, &self.lease);
        let was_playing = self.ring.phase == super::ConsumerPhase::Playing;
        let filled = self.ring.fill(&mut self.cursor, recv);
        self.events.fill_result(
            filled,
            was_playing,
            self.ring.phase.is_terminal(),
            self.session.playhead.position(),
            self.ring.validator.epoch,
        );
        filled
    }

    /// Reads interleaved PCM samples into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when the producer reports a failure or closes early.
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let recv = recv_ctx(&self.session, &self.lease);
        let pitch_bend = self.controls.pitch_bend.multiplier();
        let read = self.cursor.read(
            &mut self.ring,
            &mut self.events,
            self.session.playhead.as_ref(),
            recv,
            pitch_bend,
            buf,
        )?;
        Ok(self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read))
    }

    /// Starts a non-blocking seek to `position`.
    ///
    /// # Errors
    ///
    /// Propagates seek-layer decode errors.
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        self.controls.read_mode.store(AudioReadMode::Linear);
        self.source_range = None;
        Ok(self.begin_seek(position.into()))
    }

    fn begin_seek(&mut self, intent: SeekIntent) -> SeekOutcome {
        let position = intent.target();
        let epoch = self.session.seek.begin(intent);
        if intent.is_application_visible() {
            self.events.publish(AudioEvent::SeekLifecycle {
                seek_epoch: epoch,
                stage: SeekLifecycleStage::SeekRequest,
                location: SegmentLocation::default(),
            });
        }
        if let Some(wake) = &self.session.peer_wake {
            wake.notify_now();
        }
        self.session.preload_gate.rearm();
        self.events.reset_underrun();
        self.ring.begin_seek_epoch(epoch, &mut self.cursor);
        wake_worker(self.lease.worker.as_ref());

        trace!(?position, epoch, "seek initiated via seek state");
        match self.session.playhead.duration() {
            Some(duration) if position >= duration => {
                debug_assert!(position >= duration);
                SeekOutcome::PastEof {
                    duration,
                    target: position,
                }
            }
            _ => {
                debug_assert!(
                    self.session
                        .playhead
                        .duration()
                        .is_none_or(|duration| position <= duration)
                );
                SeekOutcome::Landed {
                    target: position,
                    landed_at: position,
                }
            }
        }
    }

    #[must_use]
    /// Returns the current output PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.cursor.spec()
    }

    fn request_source_range(
        &mut self,
        range: SourceRange,
    ) -> Result<SourceRangeRequest, SourceRangeError> {
        self.source_range = None;
        self.controls.read_mode.store(AudioReadMode::Bounded);
        self.ring.preloaded = true;
        let position = duration_for_frames(self.spec().sample_rate.get(), range.start().get());
        if matches!(
            self.begin_seek(SeekIntent::Reposition(position)),
            SeekOutcome::PastEof { .. }
        ) {
            self.controls.read_mode.store(AudioReadMode::Linear);
            return Err(SourceRangeError::PastEof);
        }
        let request = SourceRangeRequest::new(range, self.session.seek_obs.epoch());
        self.source_range = Some(SourceRangeCursor {
            request,
            next_frame: range.start().get(),
        });
        Ok(request)
    }

    fn read_source_range(
        &mut self,
        request: SourceRangeRequest,
        output: &mut [f32],
    ) -> Result<SourceRangeReadOutcome, SourceRangeError> {
        self.validate_source_range_request(request, output.len())?;
        let range = request.range();
        let channels = usize::from(self.spec().channels);
        let end = range.end().get();
        let mut next_frame = self
            .source_range
            .as_ref()
            .ok_or(SourceRangeError::RequestMismatch)?
            .next_frame;

        while next_frame < end {
            let chunk = self
                .ring
                .take_available(recv_ctx(&self.session, &self.lease));
            let Some(chunk) = chunk else {
                self.update_source_range_cursor(request, next_frame)?;
                return match self.ring.phase {
                    super::ConsumerPhase::AtEof => Ok(SourceRangeReadOutcome::Eof),
                    super::ConsumerPhase::Failed => Err(SourceRangeError::SourceFailed),
                    _ => Ok(SourceRangeReadOutcome::Pending),
                };
            };
            let copied =
                copy_source_range_chunk(&chunk, self.spec(), range, next_frame, channels, output);
            self.ring.discard(chunk);
            match copied? {
                SourceRangeChunkCopy::Skip => continue,
                SourceRangeChunkCopy::Copied {
                    next_frame: copied_to,
                } => {
                    next_frame = copied_to;
                }
            }
        }

        self.update_source_range_cursor(request, next_frame)?;
        let frames =
            usize::try_from(range.len()).map_err(|_| SourceRangeError::ArithmeticOverflow)?;
        Ok(SourceRangeReadOutcome::Ready { frames })
    }

    fn update_source_range_cursor(
        &mut self,
        request: SourceRangeRequest,
        next_frame: u64,
    ) -> Result<(), SourceRangeError> {
        let cursor = self
            .source_range
            .as_mut()
            .filter(|cursor| cursor.request == request)
            .ok_or(SourceRangeError::RequestMismatch)?;
        cursor.next_frame = next_frame;
        Ok(())
    }

    fn validate_source_range_request(
        &self,
        request: SourceRangeRequest,
        output_len: usize,
    ) -> Result<(), SourceRangeError> {
        if request.seek_epoch() != self.session.seek_obs.epoch()
            || request.seek_epoch() != self.ring.validator.epoch
        {
            return Err(SourceRangeError::StaleRequest);
        }
        if self
            .source_range
            .as_ref()
            .is_none_or(|cursor| cursor.request != request)
        {
            return Err(SourceRangeError::RequestMismatch);
        }
        let frames = usize::try_from(request.range().len())
            .map_err(|_| SourceRangeError::ArithmeticOverflow)?;
        let expected = frames
            .checked_mul(usize::from(self.spec().channels))
            .ok_or(SourceRangeError::ArithmeticOverflow)?;
        if output_len != expected {
            return Err(SourceRangeError::OutputSizeMismatch {
                expected,
                actual: output_len,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceRangeChunkCopy {
    Skip,
    Copied { next_frame: u64 },
}

fn copy_source_range_chunk(
    chunk: &PcmChunk,
    spec: PcmSpec,
    range: SourceRange,
    next_frame: u64,
    channels: usize,
    output: &mut [f32],
) -> Result<SourceRangeChunkCopy, SourceRangeError> {
    if chunk.spec() != spec {
        return Err(SourceRangeError::SpecMismatch);
    }

    let chunk_frames =
        usize::try_from(chunk.meta.frames).map_err(|_| SourceRangeError::ArithmeticOverflow)?;
    let expected_chunk_samples = chunk_frames
        .checked_mul(channels)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    if chunk.samples.len() != expected_chunk_samples {
        return Err(SourceRangeError::ChunkSampleCountMismatch {
            expected: expected_chunk_samples,
            actual: chunk.samples.len(),
        });
    }

    let chunk_start = chunk.meta.frame_offset;
    let chunk_end = chunk_start
        .checked_add(u64::from(chunk.meta.frames))
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    if chunk_end <= next_frame {
        return Ok(SourceRangeChunkCopy::Skip);
    }
    if chunk_start > next_frame {
        return Err(SourceRangeError::Discontinuous {
            expected: next_frame,
            actual: chunk_start,
        });
    }

    let copy_end = chunk_end.min(range.end().get());
    let source_frame_start = usize::try_from(next_frame - chunk_start)
        .map_err(|_| SourceRangeError::ArithmeticOverflow)?;
    let copy_frames =
        usize::try_from(copy_end - next_frame).map_err(|_| SourceRangeError::ArithmeticOverflow)?;
    let source_sample_start = source_frame_start
        .checked_mul(channels)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    let copy_samples = copy_frames
        .checked_mul(channels)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    let source_sample_end = source_sample_start
        .checked_add(copy_samples)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    let output_frame_start = usize::try_from(next_frame - range.start().get())
        .map_err(|_| SourceRangeError::ArithmeticOverflow)?;
    let output_sample_start = output_frame_start
        .checked_mul(channels)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    let output_sample_end = output_sample_start
        .checked_add(copy_samples)
        .ok_or(SourceRangeError::ArithmeticOverflow)?;
    if source_sample_end > chunk.samples.len() {
        return Err(SourceRangeError::ChunkSampleCountMismatch {
            expected: source_sample_end,
            actual: chunk.samples.len(),
        });
    }
    if output_sample_end > output.len() {
        return Err(SourceRangeError::OutputSizeMismatch {
            expected: output_sample_end,
            actual: output.len(),
        });
    }
    output[output_sample_start..output_sample_end]
        .copy_from_slice(&chunk.samples[source_sample_start..source_sample_end]);
    Ok(SourceRangeChunkCopy::Copied {
        next_frame: copy_end,
    })
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmRead for Audio<S> {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.ring.preloaded = true;
        let chunk = if let Some(chunk) = self.ring.take_buffered() {
            Some(chunk)
        } else {
            let was_playing = self.ring.phase == super::ConsumerPhase::Playing;
            let chunk = self
                .ring
                .recv_valid_chunk(recv_ctx(&self.session, &self.lease));
            self.events.fill_result(
                chunk.is_some(),
                was_playing,
                self.ring.phase.is_terminal(),
                self.session.playhead.position(),
                self.ring.validator.epoch,
            );
            chunk
        };
        let Some(chunk) = chunk else {
            return chunk_outcome(self.ring.phase, self.position());
        };
        self.cursor.begin_chunk(&chunk);
        self.ring.promote_playing();
        self.session
            .playhead
            .advance(&kithara_stream::ChunkPosition::from(&chunk.meta));
        Ok(ChunkOutcome::Chunk(chunk))
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Self::read(self, buf)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let pitch_bend = self.controls.pitch_bend.multiplier();
        let read = self.cursor.read_planar(
            &mut self.ring,
            &mut self.events,
            self.session.playhead.as_ref(),
            recv_ctx(&self.session, &self.lease),
            pitch_bend,
            output,
        )?;
        Ok(self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read))
    }

    fn spec(&self) -> PcmSpec {
        Self::spec(self)
    }

    fn position(&self) -> Duration {
        self.position()
    }

    fn decoded_frontier(&self) -> Duration {
        self.session.playhead.decoded_frontier()
    }

    fn read_source_range(
        &mut self,
        request: SourceRangeRequest,
        output: &mut [f32],
    ) -> Result<SourceRangeReadOutcome, SourceRangeError> {
        Self::read_source_range(self, request, output)
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmSession for Audio<S> {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.abr_handle()
    }

    fn event_bus(&self) -> &EventBus {
        self.events.bus()
    }

    fn metadata(&self) -> &TrackMetadata {
        self.metadata()
    }

    fn preload_epoch(&self) -> u64 {
        self.session.seek_obs.epoch()
    }

    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        Some(self.session.preload_gate.clone())
    }

    fn duration(&self) -> Option<Duration> {
        self.duration()
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmControl for Audio<S> {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Self::seek(self, position)
    }

    fn preload(&mut self) -> Result<(), DecodeError> {
        Self::preload(self)
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        let previous = self
            .controls
            .host_sample_rate
            .swap(sample_rate.get(), Ordering::AcqRel);
        if previous != sample_rate.get() {
            wake_worker(self.lease.worker.as_ref());
        }
    }

    fn set_playback_rate(&self, rate: f32) {
        if let Some(controls) = &self.controls.stretch {
            controls.set_speed(rate);
        } else {
            self.controls.playback_rate.store(rate, Ordering::Relaxed);
        }
    }

    fn set_transport_bend(&self, bend: f32) {
        self.controls.pitch_bend.set_bend(bend);
    }

    fn set_service_class(&self, class: ServiceClass) {
        self.controls.service_class.store(class);
        wake_worker(self.lease.worker.as_ref());
    }

    fn request_source_range(
        &mut self,
        range: SourceRange,
    ) -> Result<SourceRangeRequest, SourceRangeError> {
        Self::request_source_range(self, range)
    }
}

fn recv_ctx<'a>(session: &'a Session, lease: &'a WorkerLease) -> RecvCtx<'a> {
    RecvCtx {
        cancel: lease.cancel.as_ref(),
        worker: lease.worker.as_ref(),
        abr: session.abr_handle.as_ref(),
    }
}

fn wake_worker(worker: Option<&AudioWorkerHandle>) {
    if let Some(worker) = worker {
        worker.wake();
    }
}

fn chunk_outcome(
    phase: super::ConsumerPhase,
    position: Duration,
) -> Result<ChunkOutcome, DecodeError> {
    match phase {
        super::ConsumerPhase::AtEof => Ok(ChunkOutcome::Eof { position }),
        super::ConsumerPhase::Failed => Err(DecodeError::Io {
            source: IoError::other("pcm channel closed / producer failed"),
        }),
        super::ConsumerPhase::SeekPending { .. } => Ok(ChunkOutcome::Pending {
            position,
            reason: PendingReason::SeekInProgress,
        }),
        _ => Ok(ChunkOutcome::Pending {
            position,
            reason: PendingReason::Buffering,
        }),
    }
}

fn channel_closed_during_preload() -> DecodeError {
    DecodeError::Io {
        source: IoError::other("pcm channel closed during preload"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicU32, AtomicU64},
    };

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta};
    use kithara_platform::sync::Arc;
    use kithara_stream::{PlayheadState, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::{Fetch, Inlet, Outlet, ThreadWake, connect, ring::RingParts};

    struct AudioFixture {
        audio: Audio<()>,
        data_tx: Outlet<Fetch<PcmChunk>>,
        trash_rx: Inlet<PcmChunk>,
    }

    impl Default for AudioFixture {
        fn default() -> Self {
            let (data_tx, data_rx) = connect::<Fetch<PcmChunk>>(4, None);
            let (trash_tx, trash_rx) = connect::<PcmChunk>(8, None);
            let epoch = Arc::new(AtomicU64::new(0));
            let ring = RingConsumer::new(RingParts {
                pcm_rx: data_rx,
                trash_tx,
                reader_wake: Arc::new(ThreadWake::default()),
                epoch,
                block_on_underrun: false,
            });
            let seek_state = Arc::new(SeekState::new());
            let seek: Arc<dyn SeekControl> = seek_state.clone();
            let seek_obs: Arc<dyn SeekObserve> = seek_state;
            let playhead: Arc<dyn PlayheadWrite> = Arc::new(PlayheadState::new());
            let pcm_pool = PcmPool::default().clone();
            let bus = EventBus::default();
            let emit = AudioEvents::deferred(&bus);
            let spec = PcmSpec::new(
                2,
                NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
            );
            Self {
                audio: Audio::from(AudioParts {
                    lease: WorkerLease {
                        cancel: None,
                        track_id: None,
                        worker: None,
                        is_standalone: false,
                    },
                    ring,
                    session: Session {
                        playhead,
                        preload_gate: Arc::new(PreloadGate::default()),
                        seek,
                        seek_obs,
                        metadata: TrackMetadata::default(),
                        abr_handle: None,
                        peer_wake: None,
                    },
                    controls: Controls {
                        host_sample_rate: Arc::new(AtomicU32::new(0)),
                        playback_rate: Arc::new(AtomicF32::new(1.0)),
                        pitch_bend: PitchBend::default(),
                        stretch: None,
                        service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
                        read_mode: Arc::new(AtomicAudioReadMode::default()),
                    },
                    pcm_pool,
                    spec,
                    emit,
                    marker: PhantomData,
                }),
                data_tx,
                trash_rx,
            }
        }
    }

    fn chunk(frame_offset: u64, frames: u32, samples: &[f32]) -> PcmChunk {
        let spec = PcmSpec::new(
            2,
            NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"),
        );
        PcmChunk::new(
            PcmMeta {
                spec,
                frames,
                frame_offset,
                ..PcmMeta::default()
            },
            PcmPool::default().attach(samples.to_vec()),
        )
    }

    #[kithara::test]
    fn seek_rearms_preload_gate_before_worker_refill() {
        let mut fixture = AudioFixture::default();
        fixture.audio.session.preload_gate.signal_epoch(0);
        assert!(fixture.audio.session.preload_gate.is_ready());
        fixture
            .audio
            .seek(Duration::from_millis(250))
            .expect("seek should arm epoch");
        assert!(!fixture.audio.session.preload_gate.is_ready());
    }

    #[kithara::test]
    fn transport_bend_control_updates_consume_rate() {
        let fixture = AudioFixture::default();

        PcmControl::set_transport_bend(&fixture.audio, 1.02);

        assert_eq!(fixture.audio.controls.pitch_bend.multiplier(), 1.02);
    }

    #[kithara::test]
    fn bounded_range_copies_exactly_across_decoder_chunks() {
        let mut fixture = AudioFixture::default();
        let request = fixture
            .audio
            .request_source_range(SourceRange::try_from(2..6).expect("valid source range"))
            .expect("bounded request starts");
        fixture
            .data_tx
            .try_push(Fetch::data(
                chunk(0, 4, &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]),
                request.seek_epoch(),
            ))
            .expect("first chunk fits");
        fixture
            .data_tx
            .try_push(Fetch::data(
                chunk(4, 4, &[8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]),
                request.seek_epoch(),
            ))
            .expect("second chunk fits");
        let mut output = [0.0; 8];

        let outcome = fixture
            .audio
            .read_source_range(request, &mut output)
            .expect("bounded read succeeds");

        assert_eq!(outcome, SourceRangeReadOutcome::Ready { frames: 4 });
        assert_eq!(output, [4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
        assert!(fixture.trash_rx.try_pop().is_some());
        assert!(fixture.trash_rx.try_pop().is_some());
    }

    #[kithara::test]
    fn bounded_range_recycles_a_malformed_chunk_before_returning_error() {
        let mut fixture = AudioFixture::default();
        let request = fixture
            .audio
            .request_source_range(SourceRange::try_from(0..2).expect("valid source range"))
            .expect("bounded request starts");
        fixture
            .data_tx
            .try_push(Fetch::data(chunk(0, 2, &[0.0, 1.0]), request.seek_epoch()))
            .expect("malformed chunk fits");
        let mut output = [0.0; 4];

        let error = fixture
            .audio
            .read_source_range(request, &mut output)
            .expect_err("malformed chunk must fail");

        assert!(matches!(
            error,
            SourceRangeError::ChunkSampleCountMismatch {
                expected: 4,
                actual: 2
            }
        ));
        assert!(fixture.trash_rx.try_pop().is_some());
    }

    #[kithara::test]
    fn ordinary_seek_invalidates_a_bounded_request_and_restores_linear_mode() {
        let mut fixture = AudioFixture::default();
        let request = fixture
            .audio
            .request_source_range(SourceRange::try_from(0..2).expect("valid source range"))
            .expect("bounded request starts");

        fixture
            .audio
            .seek(Duration::from_millis(10))
            .expect("ordinary seek starts");
        let error = fixture
            .audio
            .read_source_range(request, &mut [0.0; 4])
            .expect_err("old bounded request must be stale");

        assert!(matches!(error, SourceRangeError::StaleRequest));
        assert_eq!(
            fixture.audio.controls.read_mode.load(),
            AudioReadMode::Linear
        );
    }

    #[kithara::test]
    fn bounded_range_reports_eof_without_replaying_partial_pcm() {
        let mut fixture = AudioFixture::default();
        let request = fixture
            .audio
            .request_source_range(SourceRange::try_from(0..4).expect("valid source range"))
            .expect("bounded request starts");
        fixture
            .data_tx
            .try_push(Fetch::data(
                chunk(0, 2, &[1.0, 2.0, 3.0, 4.0]),
                request.seek_epoch(),
            ))
            .expect("partial chunk fits");
        fixture
            .data_tx
            .try_push(Fetch::eof(request.seek_epoch()))
            .expect("eof marker fits");
        let mut output = [0.0; 8];

        let outcome = fixture
            .audio
            .read_source_range(request, &mut output)
            .expect("natural eof is not a decode failure");

        assert_eq!(outcome, SourceRangeReadOutcome::Eof);
        assert_eq!(output, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(fixture.trash_rx.try_pop().is_some());
    }
}
