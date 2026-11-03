use std::num::NonZeroU32;

use firewheel::{
    StreamInfo,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_command::ScopeId;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use super::DeckMixer;
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
}

pub(in crate::rt) struct OfflineProcessor<E: SessionInbox> {
    mixer: Option<Box<DeckMixer<E>>>,
    to_owner: HeapProd<Box<DeckMixer<E>>>,
    from_owner: HeapCons<(Box<DeckMixer<E>>, usize)>,
    remaining: usize,
    sample_rate: Option<NonZeroU32>,
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
                sample_rate: None,
            },
        )
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
        let frames = mixer.prepare_offline(inbox.scope(self.scope), start, frames, sample_rate);
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
    pub(in crate::rt) fn install(
        &mut self,
        mixer: Box<DeckMixer<E>>,
    ) -> Result<(), OfflineDeckError> {
        self.to_owner
            .try_push(mixer)
            .map_err(|_| OfflineDeckError::TurnInFlight)
    }
}

impl<E: SessionInbox> AudioNodeProcessor for OfflineProcessor<E> {
    fn new_stream(&mut self, info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = Some(info.sample_rate);
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
        if let Some(rate) = self.sample_rate {
            mixer.update_sample_rate(rate);
        }
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
            rate,
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
}
