mod access;
mod artifact;
mod build;
mod config;
mod lane;
mod prepare;
mod reader;
mod resampler;
mod source;

pub use artifact::{
    ArtifactDocument, ArtifactFetch, ArtifactLoadError, ArtifactSource, Cover, MAX_ARTIFACT_BYTES,
};
pub use config::ResourceConfig;
pub use lane::ResourceLane;
pub use prepare::{ResourcePrep, ResourcePrepPatch};
#[cfg(feature = "mock")]
pub(crate) use reader::mock as source_mock;
pub use reader::{OpenedTrack, Resource, ResourceLoad};
pub use resampler::PlaybackResamplerBackend;
pub use source::{ResourceSrc, SourceType};
