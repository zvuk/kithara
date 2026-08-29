use std::{
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::SamplePool;
use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{
    DeferredWake, PlayheadWrite, SeekControl, SeekObserve, SeekPrepare, WorkerWake,
};

use super::{
    AudioControl, AudioRead, AudioSession, ChunkOutcome, DecodeError, PendingReason, PreloadGate,
    PreparedAudioLane, ReadOutcome, SeekOutcome, chunk_position,
    cursor::ChunkCursor,
    event::AudioEvents,
    ring::{RecvCtx, RingConsumer},
    seek::{SeekHandle, SeekHandleParts},
};
use crate::traits::SeekBegin;

/// Pull-based PCM facade over a bounded producer ring.
pub struct Audio<S> {
    events: AudioEvents,
    cursor: ChunkCursor,
    controls: Controls,
    _marker: PhantomData<S>,
    ring: RingConsumer,
    runtime: AudioRuntime,
    session: Session,
}

/// Worker-independent audio reader and its still-concrete producer lane.
///
/// `kithara-play::PlayWorker` registers the task before exposing the reader.
#[doc(hidden)]
pub struct PreparedAudio<R, P> {
    reader: R,
    lane: PreparedAudioLane<P>,
}

impl<R, P> PreparedAudio<R, P> {
    pub(super) const fn new(reader: R, lane: PreparedAudioLane<P>) -> Self {
        Self { reader, lane }
    }

    /// Compose the reader facade and its still-concrete producer atomically.
    #[doc(hidden)]
    #[must_use]
    pub fn map<R2, P2, F>(self, map: F) -> PreparedAudio<R2, P2>
    where
        F: FnOnce(R, P) -> (R2, P2),
    {
        let (reader, lane) = self.lane.map_source_with(self.reader, map);
        PreparedAudio { reader, lane }
    }
}

impl<R, P> From<PreparedAudio<R, P>> for (R, PreparedAudioLane<P>) {
    fn from(prepared: PreparedAudio<R, P>) -> Self {
        (prepared.reader, prepared.lane)
    }
}

pub(super) struct AudioParts<S> {
    pub(super) emit: Arc<kithara_events::DeferredBus<kithara_events::Event>>,
    pub(super) controls: Controls,
    pub(super) sample_pool: SamplePool,
    pub(super) spec: AudioSpec,
    pub(super) marker: PhantomData<S>,
    pub(super) ring: RingConsumer,
    pub(super) runtime: AudioRuntime,
    pub(super) session: Session,
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
}

pub(super) struct AudioRuntime {
    pub(super) cancel: CancelToken,
    pub(super) wake: Arc<dyn WorkerWake>,
}

impl Drop for AudioRuntime {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl<S> From<AudioParts<S>> for Audio<S> {
    fn from(parts: AudioParts<S>) -> Self {
        Self {
            runtime: parts.runtime,
            ring: parts.ring,
            cursor: ChunkCursor::new(&parts.sample_pool, parts.spec),
            events: AudioEvents::new(parts.emit),
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
        let recv = recv_ctx(&self.session, &self.runtime);
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
    pub const fn is_preloaded(&self) -> bool {
        self.ring.preloaded
    }

    #[must_use]
    /// Returns track metadata.
    pub const fn metadata(&self) -> &TrackMetadata {
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
                return Err(DecodeError::audio_stream("preload", source));
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
        let recv = recv_ctx(&self.session, &self.runtime);
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
        let outcome = self.seek_handle().begin(position);
        self.sync_seek();
        Ok(outcome)
    }

    /// Control-plane handle that begins a seek without touching the reader.
    ///
    /// The blocking half of a seek — event publish, peer nudge, worker wake — lives here so a
    /// caller on an audio callback can hand it off to the control thread and keep only
    /// [`sync_seek`](Self::sync_seek).
    #[must_use]
    pub fn seek_handle(&self) -> Arc<dyn SeekBegin> {
        Arc::new(SeekHandle::new(SeekHandleParts {
            bus: self.events.bus().clone(),
            peer_wake: self.session.peer_wake.clone(),
            seek_prepare: self.session.seek_prepare.clone(),
            playhead: Arc::clone(&self.session.playhead),
            preload_gate: Arc::clone(&self.session.preload_gate),
            seek: Arc::clone(&self.session.seek),
            wake: self.runtime.wake.clone(),
        }))
    }

    /// Adopt a seek epoch begun elsewhere, dropping everything buffered before it.
    ///
    /// Lock-free and allocation-free: recycled chunks go to the trash outlet and the cursor is
    /// cleared in place, so this is the only half of a seek an audio callback may run. A no-op when
    /// no new epoch was begun.
    pub fn sync_seek(&mut self) {
        let begun = self.session.seek_obs.epoch();
        if begun == self.ring.validator.epoch {
            return;
        }
        self.events.reset_underrun();
        let popped = self.ring.begin_seek_epoch(begun, &mut self.cursor);
        if popped {
            self.ring.wake_worker(Some(self.runtime.wake.as_ref()));
        }
    }

    #[must_use]
    /// Returns the current output PCM specification.
    pub fn spec(&self) -> AudioSpec {
        self.cursor.spec()
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> AudioRead for Audio<S> {
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
                .recv_valid_chunk(recv_ctx(&self.session, &self.runtime));
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
        self.session.playhead.advance(&chunk_position(&chunk.meta));
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
            recv_ctx(&self.session, &self.runtime),
            output,
        )?;
        Ok(self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read))
    }

    fn spec(&self) -> AudioSpec {
        Self::spec(self)
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> AudioSession for Audio<S> {
    delegate::delegate! {
        to self {
            fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn duration(&self) -> Option<Duration>;
            fn is_preloaded(&self) -> bool;
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

impl<S: kithara_platform::maybe_send::MaybeSend> AudioControl for Audio<S> {
    fn preload(&mut self) -> Result<(), DecodeError> {
        Self::preload(self)
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Self::seek(self, position)
    }

    fn seek_handle(&self) -> Option<Arc<dyn SeekBegin>> {
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
            self.runtime.wake.defer();
        }
    }
}

fn recv_ctx<'a>(session: &'a Session, runtime: &'a AudioRuntime) -> RecvCtx<'a> {
    RecvCtx {
        cancel: Some(&runtime.cancel),
        worker: Some(runtime.wake.as_ref()),
        abr: session.abr_handle.as_ref(),
    }
}

fn chunk_outcome(
    phase: super::ConsumerPhase,
    position: Duration,
) -> Result<ChunkOutcome, DecodeError> {
    match phase {
        super::ConsumerPhase::AtEof => Ok(ChunkOutcome::Eof { position }),
        super::ConsumerPhase::Failed { source: failure } => {
            Err(DecodeError::audio_stream("chunk read", failure))
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, AtomicU64};

    use kithara_bufpool::SamplePool;
    use kithara_platform::{CancelScope, sync::Arc};
    use kithara_signal::{AudioChunk, AudioChunkInfo};
    use kithara_stream::{PlayheadState, SeekState, WorkerWake};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode,
        audio::{Fetch, ThreadWake, connect, ring::RingParts},
    };

    struct TestWorkerWake;

    impl WorkerWake for TestWorkerWake {
        fn wake(&self) {}

        fn defer(&self) {}
    }

    struct AudioFixture {
        audio: Audio<()>,
    }

    impl Default for AudioFixture {
        fn default() -> Self {
            let (_data_tx, data_rx) = connect::<Fetch<AudioChunk>>(1, None);
            let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
            let epoch = Arc::new(AtomicU64::new(0));
            let ring = RingConsumer::new(RingParts {
                trash_tx,
                epoch,
                audio_rx: data_rx,
                reader_wake: Arc::new(ThreadWake::default()),
                block_on_underrun: false,
                consumer_wake_mode: ConsumerWakeMode::RealtimeDeferred,
            });
            let seek_state = Arc::new(SeekState::new());
            let seek: Arc<dyn SeekControl> = seek_state.clone();
            let seek_obs: Arc<dyn SeekObserve> = seek_state;
            let playhead: Arc<dyn PlayheadWrite> = Arc::new(PlayheadState::new());
            let sample_pool = SamplePool::default();
            let bus = EventBus::default();
            let emit = AudioEvents::deferred(&bus);
            Self {
                audio: Audio::from(AudioParts {
                    ring,
                    sample_pool,
                    emit,
                    runtime: AudioRuntime {
                        cancel: CancelScope::new(None).token(),
                        wake: Arc::new(TestWorkerWake),
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
                    },
                    spec: AudioChunkInfo::default().spec,
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
