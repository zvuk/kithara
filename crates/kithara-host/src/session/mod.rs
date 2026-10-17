//! Concrete session state, graph dispatch, and platform backends.

pub(crate) mod decks;
pub(crate) mod dispatch;
pub(crate) mod graph;
pub(crate) mod protocol;
pub(crate) mod queue;
pub(crate) mod state;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native_engine;
#[cfg(feature = "offline")]
pub(crate) mod offline;

#[cfg(target_arch = "wasm32")]
pub(crate) mod web;

pub(crate) use protocol::{HostDispatcher, SessionError, SessionSampleRate, ask};
pub(crate) use queue::HostProtocol;
pub(crate) use state::{HostRoot, RootView};
pub use transport::TransportEvent;
pub(crate) use transport::{Span, applied_spans};
