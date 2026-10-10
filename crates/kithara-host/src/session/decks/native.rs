#[cfg(not(target_arch = "wasm32"))]
use std::task::Wake;
use std::task::Waker;

use kithara_platform::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::sync::Weak;
use kithara_play::{HostedDeck, PlayError};

use crate::{DeckId, HostOwner};

/// A deck mailbox or dispatcher receipt wake, never ownership transfer.
#[derive(Clone, Copy)]
pub(crate) enum DeckMsg {
    Drain(DeckId),
    Receipts,
}

impl DeckMsg {
    pub(crate) fn run<S, O: HostOwner<S>>(self, owner: &mut O) {
        match self {
            Self::Receipts => owner.begin_pass(),
            Self::Drain(id) => {
                if let Err(error) =
                    owner.with_deck(id, &mut |deck, out, pass| deck.drain(pass, out))
                {
                    tracing::warn!(?id, %error, "host deck drain failed");
                }
            }
        }
    }
}

pub(crate) trait DeckInbox:
    kithara_platform::maybe_send::MaybeSend + kithara_platform::maybe_send::MaybeSync + 'static
{
    fn post(&self, message: DeckMsg) -> Result<(), PlayError>;
    #[cfg(target_arch = "wasm32")]
    fn waker(self: Arc<Self>, message: DeckMsg) -> Waker;
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct DeckWake {
    message: DeckMsg,
    inbox: Weak<dyn DeckInbox>,
}

#[cfg(target_arch = "wasm32")]
pub(crate) struct DeckWake;

impl DeckWake {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn waker(inbox: &Arc<dyn DeckInbox>, message: DeckMsg) -> Waker {
        inbox.clone().waker(message)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn waker(inbox: &Arc<dyn DeckInbox>, message: DeckMsg) -> Waker {
        Waker::from(Arc::new(Self {
            message,
            inbox: Arc::downgrade(inbox),
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Wake for DeckWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(inbox) = self.inbox.upgrade() {
            drop(inbox.post(self.message));
        }
    }
}
