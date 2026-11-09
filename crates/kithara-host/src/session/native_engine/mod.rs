mod callback;
mod client;
mod config;
mod device;
mod engine;
mod start;
mod stream;

use std::{
    num::NonZeroU32,
    task::{Wake, Waker},
};

use config::cpal_config;
use device::AcquiredOutput;
pub(crate) use engine::{EngineMsg, SessionClient};
use engine::{SessionWake, engine_thread};
use kithara_bufpool::HasPool;
use kithara_command::{Live, Ticket, mailbox};
use kithara_platform::{
    maybe_send::MaybeSend,
    sync::{Arc, Mutex, mpsc},
    thread::spawn_named,
    time::Instant,
};
pub(crate) use start::spawn;
pub(crate) use stream::NativeStream;
use tracing::debug;

use super::{
    decks::{DeckInbox, DeckMsg},
    dispatch::OwnerPosts,
    protocol::{HostDispatchError, HostDispatcher, HostMailbox, HostPostbox, not_taken},
    queue::HostProtocol,
    state::{HostRoot, RootView, SessionBufferConfig, SessionState, SessionStream},
};
use crate::{HostCore, HostOwner, HostSettings, PlayError, consts, rt::SessionOutput};

#[cfg(test)]
mod tests;
