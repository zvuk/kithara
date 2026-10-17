//! One publication cut shared by a root executor and independently credited scopes.

mod build;
mod inbox;
mod sender;
#[cfg(test)]
mod tests;
mod types;

pub use build::scoped_channel;
pub use types::{Closed, OpenError, ScopeId, ScopedConfig, ScopedReceipt, StaleScope};
use types::{Item, ScopeDocket, Slot, State};
pub(super) use types::{Lifecycle, ScopeReply};

pub use self::{
    inbox::ScopedInbox,
    sender::{ScopeSender, ScopedSender},
};
