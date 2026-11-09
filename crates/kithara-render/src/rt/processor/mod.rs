mod core;
#[cfg(any(test, feature = "mock"))]
pub(crate) mod mock;
mod offline;
mod shape;

pub use core::DeckMixer;
pub(in crate::rt) use core::{Deck, PreparedDeck};

pub use offline::{OfflineDeck, OfflineDeckError};
pub(in crate::rt) use offline::{OfflineProcessor, ProcessorParts};
pub use shape::{BufferGeometryError, StreamShape};

#[cfg(test)]
mod tests;
