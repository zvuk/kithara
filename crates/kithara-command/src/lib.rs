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

mod channel;
mod config;
mod live;
mod protocol;
mod receipt;

pub use channel::{Due, Inbox, SendError, Sender, channel};
pub use config::ChannelConfig;
pub use live::{Live, LiveError, SettledChange};
pub use protocol::{Batch, Clock, Protocol, Seq, Target, When};
pub use receipt::{Outcome, Receipt, Rejection};
