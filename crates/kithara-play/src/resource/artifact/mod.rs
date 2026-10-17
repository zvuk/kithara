mod document;
mod fetch;
mod prepared;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub use document::{ArtifactDocument, Cover};
pub use fetch::{ArtifactFetch, ArtifactLoadError, MAX_ARTIFACT_BYTES};
pub use prepared::ArtifactSource;
