use std::{
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_stream::{DeferredWake, PlayheadWrite, SeekControl, SeekObserve, SeekPrepare};
use portable_atomic::AtomicF32;

use super::{
    AtomicServiceClass, AudioWorkerHandle, ChunkOutcome, DecodeError, PcmControl, PcmRead,
    PcmSession, PendingReason, PreloadGate, ReadOutcome, SeekOutcome, ServiceClass,
    StretchControls, TrackId,
    cursor::ChunkCursor,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer},
    seek::{SeekHandle, SeekHandleParts},
};
use crate::traits::SeekDeclare;

/// Pull-based PCM facade backed by a shared renderer worker.
pub struct Audio<S> {
    events: AudioEvents,
    cursor: ChunkCursor,
    controls: Controls,
    _marker: PhantomData<S>,
    ring: RingConsumer,
    session: Session,
    lease: WorkerLease,
}

pub(super) struct AudioParts<S> {
    pub(super) emit: Arc<kithara_events::DeferredBus<kithara_events::Event>>,
    pub(super) controls: Controls,
    pub(super) pcm_pool: PcmPool,
    pub(super) spec: PcmSpec,
    pub(super) marker: PhantomData<S>,
    pub(super) ring: RingConsumer,
    pub(super) session: Session,
    pub(super) lease: WorkerLease,
}

pub(super) struct Session {
    pub(super) playhead: Arc<dyn PlayheadWrite>,
    pub(super) preload_gate: Arc<PreloadGate>,
    pub(super) seek: Arc<dyn SeekControl>,
    pub(super) seek_obs: Arc<dyn SeekObserve>,
    pub(super) abr_handle: Option<kithara_abr::AbrHandle>,
    pub(super) peer_wake: Option<Arc<DeferredWake>>,
    pub(super) seek_prepare: Option<Arc<dyn SeekPrepare>>,
    pub(super) metadata: TrackMetadata,
}

pub(super) struct Controls {
    pub(super) host_sample_rate: Arc<AtomicU32>,
    pub(super) playback_rate: Arc<AtomicF32>,
    pub(super) service_class: Arc<AtomicServiceClass>,
    pub(super) stretch: Option<Arc<StretchControls>>,
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

    delegate::delegate! {
        to self.session.playhead {
            #[must_use]
            /// Returns the known stream duration.
            pub fn duration(&self) -> Option<Duration>;
            #[must_use]
            /// Returns the committed playback position.
            pub fn position(&self) -> Duration;
        }
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

    /// Enables non-blocking reads and primes the first PCM chunk.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if the producer channel closes during preload.
    pub fn preload(&mut self) -> Result<(), DecodeError> {
        self.sync_seek();
        if !self.is_preloaded() {
            self.ring.preloaded = true;
        }
        if self.ring.current_chunk.is_none() && self.ring.phase != super::ConsumerPhase::AtEof {
            self.fill_buffer();
            if let super::ConsumerPhase::Failed { source } = self.ring.phase {
                return Err(channel_closed_during_preload(source));
            }
        }
        Ok(())
    }

    /// Reads interleaved PCM samples into `buf`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when the producer reports a failure or closes early.
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        self.sync_seek();
        let recv = recv_ctx(&self.session, &self.lease);
        let read = self.cursor.read(
            &mut self.ring,
            &mut self.events,
            self.session.playhead.as_ref(),
            recv,
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
        let outcome = self.seek_handle().declare(position);
        self.sync_seek();
        Ok(outcome)
    }

    /// Control-plane handle that declares a seek without touching the reader.
    ///
    /// The blocking half of a seek — event publish, peer nudge, worker wake —
    /// lives here so a caller on an audio callback can hand it off to the
    /// control thread and keep only [`sync_seek`](Self::sync_seek).
    #[must_use]
    pub fn seek_handle(&self) -> Arc<dyn SeekDeclare> {
        Arc::new(SeekHandle::new(SeekHandleParts {
            bus: self.events.bus().clone(),
            peer_wake: self.session.peer_wake.clone(),
            seek_prepare: self.session.seek_prepare.clone(),
            playhead: Arc::clone(&self.session.playhead),
            preload_gate: Arc::clone(&self.session.preload_gate),
            seek: Arc::clone(&self.session.seek),
            worker: self.lease.worker.clone(),
        }))
    }

    /// Adopt a seek epoch declared elsewhere, dropping everything buffered
    /// before it.
    ///
    /// Lock-free and allocation-free: recycled chunks go to the trash outlet
    /// and the cursor is cleared in place, so this is the only half of a seek
    /// an audio callback may run. A no-op when no new epoch was declared.
    pub fn sync_seek(&mut self) {
        let declared = self.session.seek_obs.epoch();
        if declared == self.ring.validator.epoch {
            return;
        }
        self.events.reset_underrun();
        self.ring.begin_seek_epoch(declared, &mut self.cursor);
    }

    #[must_use]
    /// Returns the current output PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.cursor.spec()
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmRead for Audio<S> {
    delegate::delegate! {
        to self.session.playhead {
            #[call(cached)]
            fn cached_span(&self) -> Duration;
            fn decoded_frontier(&self) -> Duration;
        }
    }

    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.sync_seek();
        self.ring.preloaded = true;
        let chunk = if let Some(chunk) = self.ring.current_chunk.take() {
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

    fn position(&self) -> Duration {
        self.position()
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Self::read(self, buf)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        self.sync_seek();
        let read = self.cursor.read_planar(
            &mut self.ring,
            &mut self.events,
            self.session.playhead.as_ref(),
            recv_ctx(&self.session, &self.lease),
            output,
        )?;
        Ok(self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read))
    }

    fn spec(&self) -> PcmSpec {
        Self::spec(self)
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmSession for Audio<S> {
    delegate::delegate! {
        to self {
            fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn duration(&self) -> Option<Duration>;
            fn metadata(&self) -> &TrackMetadata;
        }
    }

    fn event_bus(&self) -> &EventBus {
        self.events.bus()
    }

    fn preload_epoch(&self) -> u64 {
        self.session.seek_obs.epoch()
    }

    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        Some(self.session.preload_gate.clone())
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmControl for Audio<S> {
    fn preload(&mut self) -> Result<(), DecodeError> {
        Self::preload(self)
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Self::seek(self, position)
    }

    fn seek_handle(&self) -> Option<Arc<dyn SeekDeclare>> {
        Some(Self::seek_handle(self))
    }

    fn sync_seek(&mut self) {
        Self::sync_seek(self);
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
        super::ConsumerPhase::Failed { source: failure } => {
            Err(DecodeError::pcm_stream("chunk read", failure))
        }
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

fn channel_closed_during_preload(failure: super::FailureSource) -> DecodeError {
    DecodeError::pcm_stream("preload", failure)
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
                trash_tx,
                epoch,
                pcm_rx: data_rx,
                reader_wake: Arc::new(ThreadWake::default()),
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
                    ring,
                    pcm_pool,
                    emit,
                    lease: WorkerLease {
                        cancel: None,
                        track_id: None,
                        worker: None,
                        is_standalone: false,
                    },
                    session: Session {
                        playhead,
                        seek,
                        seek_obs,
                        preload_gate: Arc::new(PreloadGate::default()),
                        metadata: TrackMetadata::default(),
                        abr_handle: None,
                        peer_wake: None,
                        seek_prepare: None,
                    },
                    controls: Controls {
                        host_sample_rate: Arc::new(AtomicU32::new(0)),
                        playback_rate: Arc::new(AtomicF32::new(1.0)),
                        stretch: None,
                        service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
                    },
                    spec: PcmMeta::default().spec,
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
}
