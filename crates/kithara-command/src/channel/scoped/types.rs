use std::num::NonZeroU16;

use kithara_config::Config;

use crate::{
    ChannelConfig, Protocol, Receipt,
    channel::{book::Book, docket::Docket, sender::Sent},
};

/// A slot identity; retirement changes its generation before it can be reused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopeId {
    pub(super) index: u16,
    pub(super) generation: u32,
}

impl ScopeId {
    /// The slot's fixed position in this channel.
    #[must_use]
    pub fn index(self) -> u16 {
        self.index
    }

    /// The slot's incarnation, incremented at retirement.
    #[must_use]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Capacities reserved once, before any real-time operation.
#[derive(Clone, Copy, Debug, Config)]
#[config(fields(value))]
#[non_exhaustive]
pub struct ScopedConfig {
    /// Root credits and target count.
    #[config(builder(default = ChannelConfig::builder().build()))]
    pub(crate) root: ChannelConfig,
    /// Independently reusable scope slots.
    #[config(builder(default = NonZeroU16::MIN))]
    pub(crate) scopes: NonZeroU16,
    /// Credits per scope and the maximum target count of an opened scope.
    #[config(builder(default = ChannelConfig::builder().build()))]
    pub(crate) scope: ChannelConfig,
}

/// An answer routed to its own level; Closed is the last answer of a generation.
#[derive(Debug)]
pub enum ScopedReceipt<R: Protocol, M: Protocol> {
    /// An ordinary root-level receipt.
    Root(Receipt<R>),
    /// An ordinary receipt tagged by the batch's own scope generation.
    Scope(ScopeId, Receipt<M>),
    /// All batches of this scope generation have been answered and it is free.
    Closed(ScopeId),
}

/// Why a scope cannot be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// Every scope slot is open or awaiting Closed.
    #[error("all scope slots are in use")]
    Exhausted,
    /// The requested target count exceeds the preallocated maximum.
    #[error("requested {targets} scope targets, limit is {limit}")]
    Targets {
        /// The requested count.
        targets: usize,
        /// The configured maximum.
        limit: usize,
    },
}

/// The identity does not name an open scope of this generation.
#[derive(Debug, thiserror::Error)]
#[error("the scope is not open in this generation")]
pub struct StaleScope;

/// The inbox has closed the publication gate.
#[derive(Debug, thiserror::Error)]
#[error("the channel's inbox is gone")]
pub struct Closed;

pub(super) enum Item<R: Protocol, M: Protocol> {
    Root(Sent<R>),
    Scope { id: ScopeId, sent: Sent<M> },
    Close(ScopeId),
}

pub(in crate::channel) enum ScopeReply<P: Protocol> {
    Receipt { id: ScopeId, receipt: Receipt<P> },
    Closed(ScopeId),
}

pub(in crate::channel) struct Lifecycle {
    pub(in crate::channel) generation: u32,
    pub(in crate::channel) closing: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum State {
    Free,
    Open,
    Closing,
}

pub(super) struct Slot<P: Protocol> {
    pub(super) state: State,
    pub(super) generation: u32,
    pub(super) book: Book<P>,
}

pub(super) struct ScopeDocket<P: Protocol> {
    pub(super) docket: Docket<P>,
    pub(super) lifecycle: Lifecycle,
}
