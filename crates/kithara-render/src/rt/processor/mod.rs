mod core;
#[cfg(any(test, feature = "mock"))]
pub(crate) mod mock;
mod offline;
mod shape;

pub use core::DeckMixer;
pub(crate) use core::{Deck, PreparedDeck};

pub(crate) use offline::OfflineProcessor;
pub use shape::{BufferGeometryError, StreamShape};

#[cfg(test)]
mod tests;
