//! A queue is a deck: one owner drives its tracks through commands and receipts.

mod command;
mod handle;
mod hosted;
mod lifecycle;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "mock")))]
pub mod mock;
mod slots;
mod state;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod transition;
mod types;
mod view;

pub(crate) use command::QueuePostbox;

pub use self::{
    command::QueueCommand,
    state::{Queue, QueueControl},
    types::{PlaybackView, Transition},
    view::QueueSnapshot,
};
