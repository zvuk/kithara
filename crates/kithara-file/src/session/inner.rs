//! Owned shared state of a session.
//!
//! `FileInner` is **immutable after creation** in every meaningful sense:
//! all fields are either plain owned data (URL, key) or already-thread-safe
//! handles (`AssetResource`, `EventBus`, `CancellationToken`, `Arc<…>`). The
//! two genuinely mutable bits — the FSM phase and the content-type codec —
//! live in `AtomicU8` and `OnceLock` respectively, so the struct can be
//! shared as `Arc<FileInner>` without a `Mutex` in front of it.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU8, Ordering},
};

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

/// File-streaming FSM phases. Stored as `AtomicU8` for lock-free transitions
/// from the download driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum FilePhase {
    /// Ready to start the initial full-file download.
    Init = 0,
    /// Download in progress.
    Downloading = 1,
    /// File fully downloaded (or local).
    Complete = 2,
}

/// Immutable creation-time parameters for `FileInner::new`.
pub(crate) struct FileInnerParams {
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) cancel: CancellationToken,
    pub(crate) url: Url,
    pub(crate) headers: Option<Headers>,
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) key: ResourceKey,
}

/// Shared inner state for a `FileSource`. All fields are either immutable
/// (set at construction) or self-synchronizing — there is no `Mutex`.
pub(crate) struct FileInner {
    // --- creation-time params (immutable) ---
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) cancel: CancellationToken,
    pub(crate) url: Url,
    pub(crate) headers: Option<Headers>,
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) key: ResourceKey,

    // --- mutable state (no Mutex needed) ---
    /// FSM phase as `FilePhase as u8`. Lock-free transitions.
    phase: AtomicU8,
    /// Codec discovered from the HTTP `Content-Type` header on first connect.
    /// Set at most once by the download driver.
    pub(crate) content_type_codec: OnceLock<AudioCodec>,
}

impl FileInner {
    pub(crate) fn new(params: FileInnerParams, initial_phase: FilePhase) -> Self {
        let FileInnerParams {
            coord,
            res,
            bus,
            cancel,
            url,
            headers,
            backend,
            key,
        } = params;
        Self {
            coord,
            res,
            bus,
            cancel,
            url,
            headers,
            backend,
            key,
            phase: AtomicU8::new(initial_phase as u8),
            content_type_codec: OnceLock::new(),
        }
    }

    /// Lock-free FSM transition.
    pub(crate) fn set_phase(&self, phase: FilePhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    /// Mark the resource failed and evict the pre-allocated cache file.
    ///
    /// Call on any terminal download error so the file is gone from disk
    /// before the task returns — without this the clone in `FileInner.res`
    /// keeps the mmap parked in the cache directory for the full lifetime
    /// of the holding `Stream<File>`.
    pub(crate) fn fail_and_evict(&self, reason: &str) {
        self.set_phase(FilePhase::Complete);
        self.res.fail(reason.to_string());
        self.backend.remove_resource(&self.key);
    }
}
