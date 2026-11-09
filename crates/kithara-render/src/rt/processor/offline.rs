use std::num::NonZeroU32;

use firewheel::node::{
    AudioNodeProcessor, NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
};
use kithara_command::ScopeId;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use super::{DeckMixer, StreamShape};
use crate::bridge::SessionInbox;

/// Owns a stopped native mixer or an offline mixer between graph turns.
pub struct OfflineDeck<E: SessionInbox> {
    scope: ScopeId,
    mixer: Option<Box<DeckMixer<E>>>,
    prepared: Option<usize>,
    to_processor: HeapProd<(Box<DeckMixer<E>>, usize)>,
    from_processor: HeapCons<Box<DeckMixer<E>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum OfflineDeckError {
    #[error("offline mixer is not owned by the session")]
    MissingMixer,
    #[error("offline mixer turn is still in flight")]
    TurnInFlight,
    #[error("offline turn must fit its nonempty prepared PCM range")]
    InvalidFrames,
    #[error("offline turn rate {provided} differs from admitted rate {admitted}")]
    SampleRateMismatch {
        provided: NonZeroU32,
        admitted: NonZeroU32,
    },
}

pub(in crate::rt) struct OfflineProcessor<E: SessionInbox> {
    mixer: Option<Box<DeckMixer<E>>>,
    parts: Option<ProcessorParts<E>>,
    recycle: Option<HeapProd<ProcessorParts<E>>>,
    remaining: usize,
}

pub(in crate::rt) struct ProcessorParts<E: SessionInbox> {
    to_owner: HeapProd<Box<DeckMixer<E>>>,
    from_owner: HeapCons<(Box<DeckMixer<E>>, usize)>,
    native: bool,
}

impl<E: SessionInbox> OfflineDeck<E> {
    pub(in crate::rt) fn channel(scope: ScopeId) -> (Self, OfflineProcessor<E>) {
        let (to_processor, from_owner) = HeapRb::new(1).split();
        let (to_owner, from_processor) = HeapRb::new(1).split();
        (
            Self {
                scope,
                mixer: None,
                prepared: None,
                to_processor,
                from_processor,
            },
            OfflineProcessor {
                mixer: None,
                parts: Some(ProcessorParts {
                    to_owner,
                    from_owner,
                    native: false,
                }),
                recycle: None,
                remaining: 0,
            },
        )
    }

    /// Prepares every session-owned mixer before activating an offline stream.
    ///
    /// # Errors
    /// Returns the typed design or capacity refusal without changing sounding state.
    pub fn prepare_stream(&mut self, shape: StreamShape) -> Result<(), NodeError> {
        self.prepared = None;
        for mixer in self.mixer.iter_mut().chain(self.from_processor.iter_mut()) {
            mixer.prepare_stream(shape)?;
        }
        Ok(())
    }

    /// Reserves the native handoff before committing a stopped stream.
    ///
    /// # Errors
    /// Returns an ownership refusal while the previous processor still holds the mixer.
    pub fn reserve_stream(&mut self) -> Result<(), OfflineDeckError> {
        self.acquire()?;
        if self.to_processor.is_full() {
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// Returns a prepared native mixer to its stopped processor.
    ///
    /// # Errors
    /// A full channel retains the owned mixer for another admitted start.
    pub fn resume_stream(&mut self) -> Result<(), OfflineDeckError> {
        let mixer = self.mixer.take().ok_or(OfflineDeckError::MissingMixer)?;
        if let Err((mixer, _)) = self.to_processor.try_push((mixer, 0)) {
            self.mixer = Some(mixer);
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// Commits the preparation after the offline graph admits its stream.
    pub fn commit_stream(&mut self) {
        for mixer in self.mixer.iter_mut().chain(self.from_processor.iter_mut()) {
            mixer.commit_stream();
        }
    }

    /// Apply due deck commands and wait for opted-in PCM before graph processing.
    ///
    /// # Errors
    /// Returns an error if a previous graph turn did not return its mixer.
    pub fn prepare<I: SessionInbox>(
        &mut self,
        inbox: &mut I,
        start: SessionFrame,
        frames: usize,
        sample_rate: NonZeroU32,
    ) -> Result<usize, OfflineDeckError> {
        if frames == 0 {
            return Err(OfflineDeckError::InvalidFrames);
        }
        self.acquire()?;
        let mixer = self.mixer.as_mut().ok_or(OfflineDeckError::MissingMixer)?;
        if mixer.deck.sample_rate != sample_rate {
            return Err(OfflineDeckError::SampleRateMismatch {
                provided: sample_rate,
                admitted: mixer.deck.sample_rate,
            });
        }
        let frames = mixer.prepare_offline(inbox.scope(self.scope), start, frames);
        self.prepared = Some(frames);
        Ok(frames)
    }

    /// Reserves a complete offline turn before any deck transfers its payload.
    ///
    /// # Errors
    /// Returns invalid-frame, ownership or channel pressure before changing ownership.
    pub fn reserve_render(&self, frames: usize) -> Result<(), OfflineDeckError> {
        if !self
            .prepared
            .is_some_and(|prepared| frames > 0 && frames <= prepared)
        {
            return Err(OfflineDeckError::InvalidFrames);
        }
        if self.mixer.is_none() {
            return Err(OfflineDeckError::MissingMixer);
        }
        if self.to_processor.is_full() {
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// Transfers the reserved mixer for exactly `frames` graph frames.
    ///
    /// # Errors
    /// Retains the payload if the prepared turn or its reservation is unavailable.
    pub fn begin_render(&mut self, frames: usize) -> Result<(), OfflineDeckError> {
        self.reserve_render(frames)?;
        let mixer = self.mixer.take().ok_or(OfflineDeckError::MissingMixer)?;
        if let Err((mixer, _)) = self.to_processor.try_push((mixer, frames)) {
            self.mixer = Some(mixer);
            return Err(OfflineDeckError::TurnInFlight);
        }
        self.prepared = None;
        Ok(())
    }

    /// Reclaim the mixer after the graph has completed the declared turn.
    ///
    /// # Errors
    /// Returns an error if the graph failed to return the owning value.
    pub fn end_render(&mut self) -> Result<(), OfflineDeckError> {
        self.acquire()
    }

    fn acquire(&mut self) -> Result<(), OfflineDeckError> {
        if self.mixer.is_none() {
            self.mixer = Some(
                self.from_processor
                    .try_pop()
                    .ok_or(OfflineDeckError::MissingMixer)?,
            );
        }
        Ok(())
    }

    /// Reclaims a retired processor's payload once its final schedule releases it.
    pub fn reclaim_stopped(&mut self) -> bool {
        self.acquire().is_ok() || !self.to_processor.read_is_held()
    }
}

impl<E: SessionInbox> OfflineProcessor<E> {
    pub(in crate::rt) fn recycle_to(&mut self, recycle: HeapProd<ProcessorParts<E>>) {
        self.recycle = Some(recycle);
    }

    pub(in crate::rt) fn from_parts(
        parts: ProcessorParts<E>,
        recycle: HeapProd<ProcessorParts<E>>,
    ) -> Self {
        Self {
            mixer: None,
            parts: Some(parts),
            recycle: Some(recycle),
            remaining: 0,
        }
    }

    pub(in crate::rt) fn native(&mut self) {
        self.parts
            .as_mut()
            .expect("processor endpoints are owned until drop")
            .native = true;
    }

    pub(in crate::rt) fn check_install(&self) -> Result<(), OfflineDeckError> {
        if self
            .parts
            .as_ref()
            .expect("processor endpoints are owned until drop")
            .to_owner
            .is_full()
        {
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// The sole producer checked capacity before taking the constructor's inputs.
    pub(in crate::rt) fn install(&mut self, mixer: Box<DeckMixer<E>>) {
        let parts = self
            .parts
            .as_mut()
            .expect("processor endpoints are owned until drop");
        if parts.native {
            self.mixer = Some(mixer);
            return;
        }
        assert!(
            parts.to_owner.try_push(mixer).is_ok(),
            "reserved cold mixer handoff"
        );
    }

    fn return_stopped(&mut self) {
        let parts = self
            .parts
            .as_mut()
            .expect("processor endpoints are owned until drop");
        if self.mixer.is_none()
            && let Some((mixer, _)) = parts.from_owner.try_pop()
        {
            self.mixer = Some(mixer);
        }
        if let Some(mixer) = self.mixer.take()
            && let Err(mixer) = parts.to_owner.try_push(mixer)
        {
            self.mixer = Some(mixer);
        }
    }
}

impl<E: SessionInbox> AudioNodeProcessor for OfflineProcessor<E> {
    fn stream_stopped(&mut self, _context: &mut ProcStreamCtx) {
        self.return_stopped();
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let parts = self
            .parts
            .as_mut()
            .expect("processor endpoints are owned until drop");
        if self.mixer.is_none()
            && let Some((mut mixer, frames)) = parts.from_owner.try_pop()
        {
            mixer.commit_stream();
            self.mixer = Some(mixer);
            self.remaining = frames;
        }
        let Some(mixer) = &mut self.mixer else {
            let _ = extra
                .logger
                .try_error("offline session did not supply its mixer");
            return ProcessStatus::ClearAllOutputs;
        };
        if parts.native {
            return mixer.process(info, buffers, extra);
        }
        let Some(remaining) = self.remaining.checked_sub(info.frames) else {
            let _ = extra
                .logger
                .try_error("offline graph exceeded its prepared turn");
            return ProcessStatus::ClearAllOutputs;
        };
        let status = mixer.process(info, buffers, extra);
        self.remaining = remaining;
        if remaining == 0
            && let Some(mixer) = self.mixer.take()
            && let Err(mixer) = parts.to_owner.try_push(mixer)
        {
            self.mixer = Some(mixer);
            let _ = extra
                .logger
                .try_error("offline session did not reclaim its mixer");
        }
        status
    }
}

impl<E: SessionInbox> Drop for OfflineProcessor<E> {
    fn drop(&mut self) {
        self.return_stopped();
        // One unique mixer crosses this pair of single-slot channels.
        assert!(self.mixer.is_none(), "stopped mixer return was reserved");
        if let Some(mut recycle) = self.recycle.take()
            && let Some(parts) = self.parts.take()
        {
            assert!(
                recycle.try_push(parts).is_ok(),
                "one returned factory owns the endpoint reservation"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
    use kithara_test_utils::{bufpool::pools, kithara, test::usdt};

    use super::super::mock::{mixer, send};
    use crate::{
        bridge::{DeckPart, Fade, Slot},
        mock::PacketRing,
        rt::track::{PcmConsumer, PlayerResource},
    };

    #[kithara::test]
    #[case::processor_owned(false)]
    #[case::queued_resume(true)]
    fn a_native_stop_returns_one_mixer_without_a_render_roundtrip(#[case] queued: bool) {
        use firewheel::{
            log::{RealtimeLoggerConfig, realtime_logger},
            node::{ProcStore, ProcStreamCtx},
        };

        use super::*;

        let (fixture, mut ends) = mixer();
        let initial = ends.deck.snapshot.read().clone();
        let (mut owner, mut processor) = OfflineDeck::channel(fixture.mixer.scope);
        processor.native();
        processor.check_install().expect("reserved native handoff");
        processor.install(Box::new(fixture.mixer));
        assert!(
            !owner.reclaim_stopped(),
            "running processor retains its mixer"
        );
        let mut store = ProcStore::with_capacity(1);
        let (mut logger, _events) = realtime_logger(RealtimeLoggerConfig::default());
        let mut context = ProcStreamCtx {
            store: &mut store,
            logger: &mut logger,
        };
        processor.stream_stopped(&mut context);
        owner.reserve_stream().expect("stopped mixer returned");
        if queued {
            owner.resume_stream().expect("native resume reservation");
        }
        drop(processor);
        assert!(
            owner.reclaim_stopped(),
            "retirement returns an unrendered payload"
        );
        assert_eq!(*ends.deck.snapshot.read(), initial);
        assert!(owner.from_processor.is_empty());
        assert!(owner.to_processor.is_empty());
        assert!(owner.mixer.is_some());
    }

    #[kithara::test]
    fn nonblocking_preparation_leaves_commands_for_the_graph() {
        let (mut mixer, mut ends) = mixer();
        let rate = mixer.mixer.deck.sample_rate;
        let mut packets = PacketRing::new(AudioSpec::new(2, rate), Duration::from_secs(1), 1);
        let resource = PlayerResource::new(
            PcmConsumer::new(packets.receiver.take().expect("PCM receiver")),
            Arc::from("starved-device"),
            &pools(),
        )
        .expect("resource");
        let slot = Slot::new(0);
        send(
            &mut ends,
            kithara_command::When::Next,
            vec![
                DeckPart::Attach {
                    slot,
                    item_id: kithara_events::TrackId::allocate(),
                    pcm: Box::new(resource),
                    segment: SegmentId::FIRST,
                },
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            ],
        );
        mixer.inbox.0.drain();
        let trace = usdt::scope();
        let prepared = mixer.mixer.prepare_offline(
            mixer.inbox.0.scope(mixer.mixer.scope),
            SessionFrame::new(0),
            32,
        );
        assert_eq!(prepared, 32);
        assert!(
            mixer.track(slot).is_none(),
            "preparation must not start the producer before the callback"
        );
        assert!(trace.events_of("pcm_wait_started").is_empty());
        drop(trace);
        assert!(
            ends.ring.receipt().is_none(),
            "only the graph may answer the batch"
        );
    }
    #[kithara::test]
    #[case::queued_to_session(0)]
    #[case::session_owned(1)]
    #[case::queued_to_graph(2)]
    #[case::graph_owned(3)]
    fn every_offline_owner_prepares_before_rate_commit(#[case] location: usize) {
        use firewheel::{
            StreamInfo,
            log::{RealtimeLoggerConfig, realtime_logger},
            node::{AudioNodeProcessor, ProcStore, ProcStreamCtx},
        };
        use kithara_dsp::filter::FilterError;
        use kithara_effects::eq::EqError;

        use super::*;

        fn rate<E: SessionInbox>(
            owner: &OfflineDeck<E>,
            processor: &OfflineProcessor<E>,
        ) -> NonZeroU32 {
            owner
                .mixer
                .as_ref()
                .or_else(|| owner.from_processor.iter().next())
                .or(processor.mixer.as_ref())
                .map(|mixer| mixer.deck.sample_rate)
                .or_else(|| {
                    processor
                        .parts
                        .as_ref()
                        .expect("processor endpoints")
                        .from_owner
                        .iter()
                        .next()
                        .map(|(mixer, _)| mixer.deck.sample_rate)
                })
                .expect("one actual owner retains the mixer")
        }

        let (fixture, _ends) = mixer();
        let initial = fixture.mixer.deck.sample_rate;
        let (mut owner, mut processor) = OfflineDeck::channel(fixture.mixer.scope);
        processor.check_install().expect("vacant initial handoff");
        processor.install(Box::new(fixture.mixer));
        if location >= 1 {
            owner.end_render().expect("session acquires its mixer");
        }
        if location >= 2 {
            owner.prepared = Some(1);
            owner.begin_render(1).expect("graph turn handoff");
        }
        if location >= 3 {
            let (mixer, frames) = processor
                .parts
                .as_mut()
                .expect("processor endpoints")
                .from_owner
                .try_pop()
                .expect("graph acquires its mixer");
            processor.mixer = Some(mixer);
            processor.remaining = frames;
        }
        let descriptor = |sample_rate| StreamInfo {
            sample_rate,
            sample_rate_recip: f64::from(sample_rate.get()).recip(),
            prev_sample_rate: initial,
            max_block_frames: NonZeroU32::new(512).expect("prepared capacity"),
            ..StreamInfo::default()
        };
        let mut store = ProcStore::with_capacity(1);
        let (mut logger, _events) = realtime_logger(RealtimeLoggerConfig::default());
        if location >= 2 {
            processor.stream_stopped(&mut ProcStreamCtx {
                store: &mut store,
                logger: &mut logger,
            });
        }
        let refused = descriptor(NonZeroU32::new(12_000).expect("refused rate"));
        let error = owner
            .prepare_stream(StreamShape::new(
                refused.max_block_frames,
                refused.sample_rate,
            ))
            .expect_err("the same EQ refusal reaches every owning location");
        assert!(matches!(
            error.0.downcast_ref::<EqError>(),
            Some(EqError::Filter(FilterError::Parameters))
        ));
        assert_eq!(rate(&owner, &processor), initial);
        let admitted = descriptor(NonZeroU32::new(48_000).expect("admitted rate"));
        owner
            .prepare_stream(StreamShape::new(
                admitted.max_block_frames,
                admitted.sample_rate,
            ))
            .expect("prepare");
        assert_eq!(rate(&owner, &processor), initial);
        owner.commit_stream();
        assert_eq!(rate(&owner, &processor), admitted.sample_rate);
    }
}
