use std::{
    io::Error as IoError,
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmSpec, TrackMetadata};
use kithara_events::{AudioEvent, EventBus, SeekLifecycleStage, SegmentLocation};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_stream::{DeferredWake, PlayheadWrite, SeekControl, SeekObserve};
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
use crate::{SourceAudioReader, effects::transport::PitchBend};

/// Pull-based PCM facade backed by a shared renderer worker.
pub struct Audio<S> {
    lease: WorkerLease,
    ring: RingConsumer,
    cursor: ChunkCursor,
    events: AudioEvents,
    session: Session,
    controls: Controls,
    source_audio: Option<SourceAudioReader>,
    _marker: PhantomData<S>,
}

pub(super) struct AudioParts<S> {
    pub(super) lease: WorkerLease,
    pub(super) ring: RingConsumer,
    pub(super) session: Session,
    pub(super) controls: Controls,
    pub(super) source_audio: Option<SourceAudioReader>,
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
            source_audio: parts.source_audio,
            _marker: parts.marker,
        }
    }
}

impl<S> Audio<S> {
    /// Detaches the optional decoded source-audio sidecar from this reader.
    ///
    /// Stream-backed readers return it at most once. Readers without the
    /// sidecar return `None`.
    pub fn take_source_audio(&mut self) -> Option<SourceAudioReader> {
        self.source_audio.take()
    }

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
        let epoch = self.session.seek.begin(position);
        self.session.seek.mark_pending(epoch);
        self.events.publish(AudioEvent::SeekLifecycle {
            seek_epoch: epoch,
            stage: SeekLifecycleStage::SeekRequest,
            location: SegmentLocation::default(),
        });
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
                Ok(SeekOutcome::PastEof {
                    duration,
                    target: position,
                })
            }
            _ => {
                debug_assert!(
                    self.session
                        .playhead
                        .duration()
                        .is_none_or(|duration| position <= duration)
                );
                Ok(SeekOutcome::Landed {
                    target: position,
                    landed_at: position,
                })
            }
        }
    }

    #[must_use]
    /// Returns the current output PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.cursor.spec()
    }
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
    use std::sync::atomic::{AtomicU32, AtomicU64};

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta};
    use kithara_platform::sync::Arc;
    use kithara_stream::{PlayheadState, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::{Fetch, ThreadWake, connect, ring::RingParts};

    struct AudioFixture {
        audio: Audio<()>,
    }

    impl Default for AudioFixture {
        fn default() -> Self {
            let (_data_tx, data_rx) = connect::<Fetch<PcmChunk>>(1, None);
            let (trash_tx, _trash_rx) = connect::<PcmChunk>(8, None);
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
                    },
                    source_audio: None,
                    pcm_pool,
                    spec: PcmMeta::default().spec,
                    emit,
                    marker: PhantomData,
                }),
            }
        }
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
}
