use std::num::NonZeroU32;

use kithara_assets::{AssetResource, AssetSource, AssetStore, ResourceKey};
use kithara_bufpool::HasPool;
use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::EventBus;
use kithara_file::File;
use kithara_hls::Hls;
use kithara_platform::CancelToken;

use super::{ArtifactFetch, ArtifactRequest, ResourceConfig, ResourceSrc, SourceType};

impl<S, B: Default> ResourceConfig<S, B>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// The I/O an artifact document of this track is read over: the same
    /// downloader, the same asset store scope, and the same headers policy as
    /// the audio.
    #[must_use]
    pub const fn artifact_fetch(&self) -> ArtifactFetch<'_, S, B> {
        ArtifactFetch::new(self)
    }

    /// Where this track's analysis document is read from, when its settings
    /// request one.
    #[must_use]
    pub const fn analysis(&self) -> Option<&ArtifactRequest> {
        self.analysis.as_ref()
    }

    /// Where this track's encoded cover image is read from, when it has one.
    #[must_use]
    pub const fn artwork(&self) -> Option<&ResourceSrc> {
        self.artwork.as_ref()
    }

    /// Replace where this track's encoded cover image is read from.
    pub fn set_artwork(&mut self, artwork: ResourceSrc) {
        self.artwork = Some(artwork);
    }

    /// Mint a layout-owned key for a playback or derived resource.
    ///
    /// # Errors
    ///
    /// Returns an error when source detection or layout validation fails.
    pub fn asset_key(&self, resource: &AssetResource) -> Result<ResourceKey, DecodeError> {
        let source_type = SourceType::detect(&self.src)?;
        let discriminator = self.discriminator.clone();
        let source = match &source_type {
            SourceType::RemoteFile(url) | SourceType::HlsStream(url) => AssetSource::Remote {
                discriminator,
                url: url.clone(),
            },
            SourceType::LocalFile(path) => AssetSource::Local { path: path.clone() },
        };
        let scope = match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                self.store.scope::<File<S>>(&source)
            }
            SourceType::HlsStream(_) => self.store.scope::<Hls<S>>(&source),
        }
        .map_err(DecodeError::backend)?;
        scope.key(resource).map_err(DecodeError::backend)
    }

    /// Event bus attached to this resource, when one was configured.
    #[must_use]
    pub const fn bus(&self) -> Option<&EventBus> {
        self.bus.as_ref()
    }

    /// Per-track parent cancel token, when one was configured.
    #[must_use]
    pub const fn cancel(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Caller-known metadata supplied before this resource opens.
    #[must_use]
    pub const fn metadata(&self) -> Option<&TrackMetadata> {
        self.metadata.as_ref()
    }

    /// Replace caller-known metadata used ahead of decoder tags.
    pub fn set_metadata(&mut self, metadata: TrackMetadata) {
        self.metadata = Some(metadata);
    }

    /// Optional cache discriminator.
    #[must_use]
    pub fn discriminator(&self) -> Option<&str> {
        self.discriminator.as_deref()
    }

    /// Replace the event bus attached to this resource.
    pub fn set_bus(&mut self, bus: EventBus) {
        self.bus = Some(bus);
    }

    /// Replace the parent cancel token for this resource.
    pub fn set_cancel(&mut self, cancel: CancelToken) {
        self.cancel = Some(cancel);
    }

    /// Replace the rate this resource's decoder resamples onto.
    pub const fn set_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.host_sample_rate = Some(sample_rate);
    }

    /// Source parsed for this resource.
    #[must_use]
    pub const fn source(&self) -> &ResourceSrc {
        &self.src
    }

    /// Shared asset store for this resource.
    #[must_use]
    pub const fn store(&self) -> &AssetStore<S> {
        &self.store
    }
}
