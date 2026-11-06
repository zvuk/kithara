use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
    },
};
use kithara_command::ScopedInbox;
#[cfg(feature = "offline")]
use kithara_command::{LevelInbox, ScopeId};
use kithara_render::bridge::DeckProtocol;
#[cfg(feature = "offline")]
use kithara_render::bridge::SessionInbox;
use ringbuf::{HeapCons, HeapProd};
#[cfg(feature = "offline")]
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};

#[cfg(feature = "offline")]
use super::TransportProcessError;
use super::TransportState;
use crate::session::queue::HostProtocol;

pub(crate) type HostInbox = ScopedInbox<HostProtocol, DeckProtocol>;

/// The offline owner holds the sole inbox between processor turns.
#[cfg(feature = "offline")]
pub(crate) struct OfflineInbox {
    inbox: Option<HostInbox>,
    to_processor: HeapProd<(HostInbox, usize)>,
    from_processor: HeapCons<HostInbox>,
}

pub(crate) struct InboxExchange {
    pub(super) to_owner: HeapProd<HostInbox>,
    pub(super) from_owner: HeapCons<(HostInbox, usize)>,
    pub(super) remaining_frames: usize,
}

#[cfg(feature = "offline")]
impl OfflineInbox {
    pub(crate) fn new(inbox: HostInbox) -> (Self, InboxExchange) {
        let (to_processor, from_owner) = HeapRb::new(1).split();
        let (to_owner, from_processor) = HeapRb::new(1).split();
        (
            Self {
                inbox: Some(inbox),
                to_processor,
                from_processor,
            },
            InboxExchange {
                to_owner,
                from_owner,
                remaining_frames: 0,
            },
        )
    }

    pub(crate) fn drain(&mut self) -> Result<(), TransportProcessError> {
        self.inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .drain();
        Ok(())
    }

    pub(crate) fn begin_render(&mut self, frames: usize) -> Result<(), TransportProcessError> {
        let inbox = self
            .inbox
            .take()
            .ok_or(TransportProcessError::MissingState)?;
        if let Err((inbox, _)) = self.to_processor.try_push((inbox, frames)) {
            self.inbox = Some(inbox);
            return Err(TransportProcessError::MissingState);
        }
        Ok(())
    }

    pub(crate) fn end_render(&mut self) -> Result<(), TransportProcessError> {
        self.inbox = Some(
            self.from_processor
                .try_pop()
                .ok_or(TransportProcessError::MissingState)?,
        );
        Ok(())
    }

    pub(crate) fn retire_closing(&mut self) -> Result<(), TransportProcessError> {
        self.inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .retire_closing();
        Ok(())
    }
}

#[cfg(feature = "offline")]
impl SessionInbox for OfflineInbox {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
        self.inbox.as_mut()?.scope(id)
    }
}

#[cfg(feature = "offline")]
impl Drop for OfflineInbox {
    fn drop(&mut self) {
        if let Some(inbox) = self.inbox.take() {
            drop(self.to_processor.try_push((inbox, 0)));
        }
    }
}

/// Returns the inbox after all scoped deck processors finish the entire offline turn.
pub(crate) struct SessionInboxReturnNode;

impl AudioNode for SessionInboxReturnNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _configuration: &EmptyConfig,
        _cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(Self)
    }

    fn info(&self, _configuration: &EmptyConfig) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("SessionInboxReturn")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

impl AudioNodeProcessor for SessionInboxReturnNode {
    fn process(
        &mut self,
        info: &ProcInfo,
        _buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if let Some(transport) = extra.store.try_get_mut::<TransportState>()
            && let Err(error) = transport.return_inbox(info.frames)
        {
            let _ = extra.logger.try_error(error.message());
        }
        ProcessStatus::Bypass
    }
}
