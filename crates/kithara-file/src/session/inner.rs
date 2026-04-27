//! Owned mutable state shared between `FileSource` and the async download tasks.
//!
//! `FileInner` lives behind `Arc<Mutex<…>>` in `FileSource` and is mutated by
//! the download driver in [`super::download`]. `FileStreamState` is a creation
//! helper: it acquires the cache resource and primes an event bus before the
//! source is built.

use std::sync::Arc;

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_storage::ResourceExt;
use kithara_stream::AudioCodec;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{coord::FileCoord, error::SourceError};

/// Creation helper: acquire the asset resource and prepare the event bus
/// before constructing a remote `FileSource`.
#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) key: ResourceKey,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &Arc<AssetStore>,
        url: &Url,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
    ) -> Result<Self, SourceError> {
        let key = ResourceKey::from_url(url);
        let res = assets.acquire_resource(&key).map_err(SourceError::Assets)?;
        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));
        Ok(Self {
            res,
            bus,
            backend: Arc::clone(assets),
            key,
        })
    }
}

/// File-streaming FSM phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePhase {
    /// Ready to start the initial full-file download.
    Init,
    /// Download in progress.
    Downloading,
    /// File fully downloaded (or local).
    Complete,
}

/// Shared inner state for a remote `FileSource`. Held behind `Arc<Mutex<…>>`
/// and mutated by both the `Source` trait methods and the spawned download
/// tasks in [`super::download`].
pub(crate) struct FileInner {
    pub(crate) phase: FilePhase,

    // Resources
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) cancel: CancellationToken,

    // Request params
    pub(crate) url: Url,
    pub(crate) headers: Option<Headers>,

    /// Codec discovered from HTTP Content-Type.
    pub(crate) content_type_codec: Option<AudioCodec>,

    /// Owning asset store — needed so a failed download can tear down the
    /// pre-allocated mmap immediately instead of waiting for
    /// `LeaseResource::Drop` to fire at `Stream<File>`-drop time (which is
    /// typically app shutdown because `FileInner.res` holds a clone that
    /// outlives every local transaction).
    pub(crate) backend: Arc<AssetStore>,
    /// Cache key for `backend.remove_resource` on failure.
    pub(crate) key: ResourceKey,
}

impl FileInner {
    /// Mark the resource failed and evict the pre-allocated cache file.
    ///
    /// Call on any terminal download error so the file is gone from disk
    /// before the task returns — without this the clone in `FileInner.res`
    /// keeps the mmap parked in the cache directory for the full lifetime
    /// of the holding `Stream<File>`.
    pub(crate) fn fail_and_evict(&mut self, reason: &str) {
        self.phase = FilePhase::Complete;
        self.res.fail(reason.to_string());
        self.backend.remove_resource(&self.key);
    }
}
