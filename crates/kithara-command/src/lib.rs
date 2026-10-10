//! Timestamped command batches for real-time executors.
//!
//! A [`Sender`] numbers each [`Batch`] and hands it over with the moment it
//! applies at, in the executor's clock. The executor drains its [`Inbox`] once
//! per block and takes the batches due inside the block in time order. A batch
//! whose moment passed comes back [`Rejection::Late`], one whose basis no
//! longer matches comes back [`Rejection::Stale`], and every batch returns
//! whole in its [`Receipt`], so nothing is dropped on the executor's thread.
//! Draining, judging and replying never allocate: [`ChannelConfig`] sizes the
//! schedule and both rings once, and the sender's credits keep the batches in
//! flight within that capacity.
//!
//! A [`Mailbox`] carries commands to an owner that runs off the real-time
//! thread. Every [`Postbox`] clone queues into it and wakes the executor that
//! holds the owner; the owner drains the commands in the order they were
//! posted, and a post made before an executor holds the owner waits for one.
//! Each post is numbered as it is queued and answered through its [`Ticket`]:
//! applied, refused by the owner, or [`Refused::Unanswered`] when the owner
//! dropped it.

mod channel;
mod config;
mod live;
mod mailbox;
mod protocol;
mod receipt;
#[cfg(test)]
mod wakes;

pub use channel::{
    Deferred, Due, Inbox, LevelInbox, Port, SendError, Sender, Step, channel, scoped,
};
pub use config::ChannelConfig;
pub use live::{Live, LiveError, SettledChange};
pub use mailbox::{Answer, Mailbox, Post, PostError, Postbox, Refused, Ticket, mailbox};
pub use protocol::{Batch, Protocol, Seq, Target, When};
pub use receipt::{Outcome, Receipt, Rejection};
pub use scoped::{
    Closed, OpenError, ScopeId, ScopeSender, ScopedConfig, ScopedInbox, ScopedReceipt,
    ScopedSender, StaleScope, scoped_channel,
};
