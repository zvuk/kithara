mod backend;
mod client;
mod engine;
mod start;

use std::{
    num::NonZeroU32,
    task::{Wake, Waker},
};

use backend::cpal_config;
pub(crate) use engine::{EngineMsg, SessionClient};
use engine::{SessionWake, engine_thread};
use firewheel::{
    FirewheelContext,
    cpal::{CpalConfig, CpalStream},
};
use kithara_bufpool::HasPool;
use kithara_command::{Live, Ticket, mailbox};
use kithara_platform::{
    maybe_send::MaybeSend,
    sync::{Arc, Mutex, mpsc},
    thread::spawn_named,
    time::Instant,
};
pub(crate) use start::spawn;
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
