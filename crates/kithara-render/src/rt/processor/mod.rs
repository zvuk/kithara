mod core;
mod driver;
#[cfg(any(test, feature = "mock"))]
pub(crate) mod mock;
mod offline;
mod shape;

pub(in crate::rt) use core::Deck;
pub use core::DeckMixer;

pub(in crate::rt) use driver::PlayerProcessor;
pub(in crate::rt) use offline::OfflineProcessor;
pub use offline::{OfflineDeck, OfflineDeckError};
pub use shape::{BufferGeometryError, StreamShape};

#[cfg(test)]
mod tests;
