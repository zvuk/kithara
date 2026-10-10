//! A queue is a deck: one owner drives its tracks through commands and receipts.

mod command;
mod handle;
mod hosted;
mod lifecycle;
mod slots;
mod state;
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
