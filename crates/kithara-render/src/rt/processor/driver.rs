use firewheel::{
    StreamInfo,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};

use super::{DeckMixer, offline::OfflineProcessor};
use crate::bridge::SessionInbox;

pub(in crate::rt) enum PlayerProcessor<E: SessionInbox> {
    Native(Box<DeckMixer<E>>),
    Offline(OfflineProcessor<E>),
}

impl<E: SessionInbox> AudioNodeProcessor for PlayerProcessor<E> {
    fn new_stream(&mut self, info: &StreamInfo, context: &mut ProcStreamCtx) {
        match self {
            Self::Native(mixer) => mixer.new_stream(info, context),
            Self::Offline(processor) => processor.new_stream(info, context),
        }
    }

    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        match self {
            Self::Native(mixer) => mixer.process(info, buffers, extra),
            Self::Offline(processor) => processor.process(info, buffers, extra),
        }
    }
}
