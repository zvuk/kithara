use crate::{
    error::UiDocError,
    ids::SourceUri,
    source::uri::{LoadedBytes, LoadedSource, SourceResolver},
};

/// Two source layers read as one: the package a user installed over the one the
/// application ships.
///
/// Only a miss falls through. A name the upper layer holds but refuses — one
/// led out of its root, or one that would not open — is answered by that
/// refusal, because it is a defect in the package that named it and not an
/// absence the layer below can fill. Reading it as a miss would let a broken
/// package quietly wear the base package's face.
#[derive(Clone, Debug)]
pub struct OverlayResolver<Over, Under> {
    over: Over,
    under: Under,
}

impl<Over, Under> OverlayResolver<Over, Under> {
    /// Reads `over` first and `under` for what it does not hold.
    pub const fn new(over: Over, under: Under) -> Self {
        Self { over, under }
    }
}

impl<Over: SourceResolver, Under: SourceResolver> SourceResolver for OverlayResolver<Over, Under> {
    fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError> {
        match self.over.bytes(base, rel) {
            Err(UiDocError::NotFound { .. }) => self.under.bytes(base, rel),
            answer => answer,
        }
    }

    fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError> {
        match self.over.load(base, rel) {
            Err(UiDocError::NotFound { .. }) => self.under.load(base, rel),
            answer => answer,
        }
    }
}
