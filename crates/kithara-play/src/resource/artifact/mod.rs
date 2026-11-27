mod document;
mod fetch;
mod request;
mod state;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub use document::{ArtifactDocument, Cover};
pub use fetch::{ArtifactFetch, ArtifactLoadError, MAX_ARTIFACT_BYTES};
pub use request::ArtifactRequest;
pub use state::ArtifactState;
