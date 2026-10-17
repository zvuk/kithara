mod core;
#[cfg(any(test, feature = "mock"))]
pub(crate) mod mock;
mod shape;

pub(in crate::rt) use core::Deck;
pub use core::DeckMixer;

pub use shape::{BufferGeometryError, StreamShape};

#[cfg(test)]
mod tests;
