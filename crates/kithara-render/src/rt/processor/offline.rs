use std::num::NonZeroU32;

use firewheel::{
    StreamInfo,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Observer, Producer},
};

use super::DeckMixer;
use crate::bridge::{OfflineDeckError, SessionInbox};

pub(crate) struct OfflineProcessor<E: SessionInbox> {
    mixer: Option<Box<DeckMixer<E>>>,
    to_owner: HeapProd<Box<DeckMixer<E>>>,
    from_owner: HeapCons<(Box<DeckMixer<E>>, usize)>,
    remaining: usize,
    sample_rate: Option<NonZeroU32>,
}

impl<E: SessionInbox> OfflineProcessor<E> {
    pub(crate) fn new(
        to_owner: HeapProd<Box<DeckMixer<E>>>,
        from_owner: HeapCons<(Box<DeckMixer<E>>, usize)>,
    ) -> Self {
        Self {
            mixer: None,
            to_owner,
            from_owner,
            remaining: 0,
            sample_rate: None,
        }
    }

    pub(crate) fn check_install(&self) -> Result<(), OfflineDeckError> {
        if self.to_owner.is_full() {
            Err(OfflineDeckError::TurnInFlight)
        } else {
            Ok(())
        }
    }

    pub(crate) fn install(&mut self, mixer: Box<DeckMixer<E>>) -> Result<(), OfflineDeckError> {
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
