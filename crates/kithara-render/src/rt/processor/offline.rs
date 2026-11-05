use std::num::NonZeroU32;

use firewheel::{
    StreamInfo,
    node::{
        AudioNodeProcessor, NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx,
        ProcessStatus,
    },
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

/// Sole owner of an offline mixer between complete graph turns.
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
    to_owner: HeapProd<Box<DeckMixer<E>>>,
    from_owner: HeapCons<(Box<DeckMixer<E>>, usize)>,
    remaining: usize,
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
                to_owner,
                from_owner,
                remaining: 0,
            },
        )
    }

    /// Prepares every session-owned mixer before activating an offline stream.
    ///
    /// # Errors
    /// Returns the typed design or capacity refusal without changing sounding state.
    pub fn prepare_stream(&mut self, shape: StreamShape) -> Result<(), NodeError> {
        for mixer in self.mixer.iter_mut().chain(self.from_processor.iter_mut()) {
            mixer.prepare_stream(shape)?;
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

    /// Transfer the prepared mixer for exactly `frames` graph frames.
    ///
    /// # Errors
    /// Returns an error while the processor still owns the previous turn.
    pub fn begin_render(&mut self, frames: usize) -> Result<(), OfflineDeckError> {
        if !self
            .prepared
            .is_some_and(|prepared| frames > 0 && frames <= prepared)
        {
            return Err(OfflineDeckError::InvalidFrames);
        }
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
}

impl<E: SessionInbox> OfflineProcessor<E> {
    pub(in crate::rt) fn check_install(&self) -> Result<(), OfflineDeckError> {
        if self.to_owner.is_full() {
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// The sole producer checked capacity before taking the constructor's inputs.
    pub(in crate::rt) fn install(&mut self, mixer: Box<DeckMixer<E>>) {
        assert!(
            self.to_owner.try_push(mixer).is_ok(),
            "reserved cold mixer handoff"
        );
    }
}

impl<E: SessionInbox> AudioNodeProcessor for OfflineProcessor<E> {
    fn prepare_new_stream(&mut self, info: &StreamInfo) -> Result<(), NodeError> {
        let shape = StreamShape::new(info.max_block_frames, info.sample_rate);
        if let Some(mixer) = self.mixer.as_mut() {
            mixer.prepare_stream(shape)?;
        }
        for (mixer, _) in self.from_owner.iter_mut() {
            mixer.prepare_stream(shape)?;
        }
        Ok(())
    }

    fn new_stream(&mut self, _info: &StreamInfo, _context: &mut ProcStreamCtx) {
        if let Some(mixer) = self.mixer.as_mut() {
            mixer.commit_stream();
        }
        for (mixer, _) in self.from_owner.iter_mut() {
            mixer.commit_stream();
        }
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if self.mixer.is_none()
            && let Some((mixer, frames)) = self.from_owner.try_pop()
        {
            self.mixer = Some(mixer);
            self.remaining = frames;
        }
        let Some(mixer) = &mut self.mixer else {
            let _ = extra
                .logger
                .try_error("offline session did not supply its mixer");
            return ProcessStatus::ClearAllOutputs;
        };
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
            && let Err(mixer) = self.to_owner.try_push(mixer)
        {
            self.mixer = Some(mixer);
            let _ = extra
                .logger
                .try_error("offline session did not reclaim its mixer");
        }
        status
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
        let refused = descriptor(NonZeroU32::new(12_000).expect("refused rate"));
        let error = if location < 2 {
            owner.prepare_stream(StreamShape::new(
                refused.max_block_frames,
                refused.sample_rate,
            ))
        } else {
            processor.prepare_new_stream(&refused)
        }
        .expect_err("the same EQ refusal reaches every owning location");
        assert!(matches!(
            error.0.downcast_ref::<EqError>(),
            Some(EqError::Filter(FilterError::Parameters))
        ));
        assert_eq!(rate(&owner, &processor), initial);
        let admitted = descriptor(NonZeroU32::new(48_000).expect("admitted rate"));
        if location < 2 {
            owner
                .prepare_stream(StreamShape::new(
                    admitted.max_block_frames,
                    admitted.sample_rate,
                ))
                .expect("prepare");
        } else {
            processor.prepare_new_stream(&admitted).expect("prepare");
        }
        assert_eq!(rate(&owner, &processor), initial);
        let mut store = ProcStore::with_capacity(1);
        let (mut logger, _events) = realtime_logger(RealtimeLoggerConfig::default());
        if location < 2 {
            owner.commit_stream();
        } else {
            processor.new_stream(
                &admitted,
                &mut ProcStreamCtx {
                    store: &mut store,
                    logger: &mut logger,
                },
            );
        }
        assert_eq!(rate(&owner, &processor), admitted.sample_rate);
    }
}
