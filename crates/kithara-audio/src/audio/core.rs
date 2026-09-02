use std::{
    marker::PhantomData,
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{
    DeferredWake, PlayheadWrite, SeekControl, SeekObserve, SeekPrepare, WorkerWake,
};
use kithara_test_utils::kithara;

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
    pub(super) cursor: ChunkCursor,
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
        let wake_mode = parts.ring.consumer_wake_mode();
        Self {
            runtime: parts.runtime,
            ring: parts.ring,
            cursor: parts.cursor,
            events: AudioEvents::new(parts.emit, wake_mode),
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
        self.wake_for_events();
        filled
    }

    fn wake_for_events(&mut self) {
        if self.events.take_wake_pending() {
            self.ring.wake_worker(Some(self.runtime.wake.as_ref()));
        }
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
    #[kithara::measure(label = "audio.read")]
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
        let outcome = self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read);
        self.wake_for_events();
        Ok(outcome)
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
            self.ring.current_source_span = None;
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
            self.wake_for_events();
            chunk.map(|(chunk, _source_span)| chunk)
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

    #[kithara::measure]
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
        let outcome = self
            .events
            .commit_read(&self.session, self.ring.validator.epoch, read);
        self.wake_for_events();
        Ok(outcome)
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
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicU32, AtomicU64},
    };

    use kithara_events::{AudioEvent, Event, EventReceiver};
    use kithara_platform::{CancelScope, sync::Arc, tokio::sync::broadcast::error::TryRecvError};
    use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
    use kithara_stream::{PlayheadState, SeekState, WorkerWake};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        ConsumerWakeMode,
        audio::{Fetch, Outlet, ThreadWake, connect, ring::RingParts},
        test_pools::pools,
    };

    struct TestWorkerWake;

    impl WorkerWake for TestWorkerWake {
        fn wake(&self) {}

        fn defer(&self) {}
    }

    struct AudioFixture {
        audio: Audio<()>,
        data_tx: Outlet<Fetch<AudioChunk>>,
        emit: Arc<kithara_events::DeferredBus<Event>>,
    }

    impl Default for AudioFixture {
        fn default() -> Self {
            Self::with_wake_mode(ConsumerWakeMode::RealtimeDeferred)
        }
    }

    impl AudioFixture {
        fn with_wake_mode(consumer_wake_mode: ConsumerWakeMode) -> Self {
            let (data_tx, data_rx) = connect::<Fetch<AudioChunk>>(1, None);
            let (trash_tx, _trash_rx) = connect::<AudioChunk>(8, None);
            let epoch = Arc::new(AtomicU64::new(0));
            let ring = RingConsumer::new(RingParts {
                trash_tx,
                epoch,
                audio_rx: data_rx,
                reader_wake: Arc::new(ThreadWake::default()),
                block_on_underrun: false,
                consumer_wake_mode,
            });
            let seek_state = Arc::new(SeekState::new());
            let seek: Arc<dyn SeekControl> = seek_state.clone();
            let seek_obs: Arc<dyn SeekObserve> = seek_state;
            let playhead: Arc<dyn PlayheadWrite> = Arc::new(PlayheadState::new());
            let cursor = ChunkCursor::new(&pools(), AudioChunkInfo::default().spec)
                .expect("cursor scratch fits test pools");
            let bus = EventBus::default();
            let emit = AudioEvents::deferred(&bus);
            Self {
                audio: Audio::from(AudioParts {
                    ring,
                    cursor,
                    emit: Arc::clone(&emit),
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
                    marker: PhantomData,
                }),
                data_tx,
                emit,
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

    fn staged_chunk() -> AudioChunk {
        let samples = pools()
            .get_with_len::<f32>(8)
            .expect("staged samples fit test pools");
        let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate is non-zero"));
        let frames = u32::try_from(samples.len() / usize::from(spec.channels))
            .expect("fixture frame count fits u32");
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames,
                ..AudioChunkInfo::default()
            },
            samples,
        )
    }

    fn drain_seek_completions(receiver: &mut EventReceiver) -> Vec<u64> {
        let mut completions = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(envelope) => {
                    if let Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. }) =
                        envelope.event
                    {
                        completions.push(seek_epoch);
                    }
                }
                Err(TryRecvError::Empty) => return completions,
                Err(error) => panic!("event receiver failed: {error:?}"),
            }
        }
    }

    /// Begin a seek epoch and stage one chunk at that epoch, so the next read
    /// returns `Frames` and births `SeekComplete` inside `commit_read`.
    fn seek_and_stage(fixture: &mut AudioFixture) -> u64 {
        fixture.audio.ring.preloaded = true;
        fixture
            .audio
            .seek(Duration::from_millis(250))
            .expect("seek begins an epoch");
        let epoch = fixture.audio.ring.validator.epoch;
        fixture
            .data_tx
            .try_push(Fetch::data(staged_chunk(), epoch))
            .expect("staged chunk reaches the ring");
        epoch
    }

    #[kithara::test]
    fn off_rt_read_publishes_the_seek_completion_it_births() {
        let mut fixture = AudioFixture::with_wake_mode(ConsumerWakeMode::ImmediateOffRt);
        let mut receiver = fixture.audio.events.bus().subscribe();
        let epoch = seek_and_stage(&mut fixture);

        let mut buf = [0.0f32; 8];
        let outcome = fixture.audio.read(&mut buf).expect("staged read");

        assert!(matches!(outcome, ReadOutcome::Frames { .. }));
        assert_eq!(
            drain_seek_completions(&mut receiver),
            vec![epoch],
            "an ImmediateOffRt consumer runs off the real-time thread, so the SeekComplete born inside its read is on the bus when the read returns"
        );
    }

    #[kithara::test]
    fn realtime_read_leaves_its_seek_completion_for_the_shell() {
        let mut fixture = AudioFixture::default();
        let mut receiver = fixture.audio.events.bus().subscribe();
        let epoch = seek_and_stage(&mut fixture);

        let mut buf = [0.0f32; 8];
        let outcome = fixture.audio.read(&mut buf).expect("staged read");

        assert!(matches!(outcome, ReadOutcome::Frames { .. }));
        assert_eq!(
            drain_seek_completions(&mut receiver),
            Vec::<u64>::new(),
            "a RealtimeDeferred read runs on the audio callback, so its events wait for the scheduler shell"
        );

        fixture.emit.flush();
        assert_eq!(
            drain_seek_completions(&mut receiver),
            vec![epoch],
            "the shell flush delivers what the read deferred"
        );
    }
}
