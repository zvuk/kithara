use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU8, Ordering},
};

use kithara_assets::{AssetResource, AssetStore, ResourceHandle, ResourceKey};
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_stream::MediaInfo;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::segments::FileSegmentIndex;
use crate::{coord::FileCoord, error::SourceError};

/// Creation helper: acquire the asset resource and prepare the event bus
/// before constructing a remote `FileSource`.
#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) key: ResourceKey,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &Arc<AssetStore>,
        url: &Url,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
    ) -> Result<Self, SourceError> {
        let key = ResourceKey::from(url);
        let res = assets.acquire_resource(&key).map_err(SourceError::Assets)?;
        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));
        Ok(Self {
            bus,
            key,
            res,
            backend: Arc::clone(assets),
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

/// Control-plane handles shared between the source and the download driver.
pub(crate) struct FileSourceCtx {
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) cancel: CancellationToken,
    pub(crate) bus: EventBus,
}

/// Data-plane handles describing where the file lives and how to fetch it.
pub(crate) struct FileAssetCtx {
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) res: AssetResource,
    pub(crate) headers: Option<Headers>,
    pub(crate) key: ResourceKey,
    pub(crate) url: Url,
}

/// Shared inner state for a `FileSource`. All fields are either immutable
/// (set at construction) or self-synchronizing — there is no `Mutex`.
pub(crate) struct FileInner {
    pub(crate) asset: FileAssetCtx,
    pub(crate) source: FileSourceCtx,
    /// `MediaInfo` discovered from the HTTP `Content-Type` header on
    /// first connect (or sniffed from the cached bytes for local
    /// fast-path). Set at most once. Carries both codec and container
    /// so downstream Apple/Android dispatch can pick a backend without
    /// re-probing the bytes.
    pub(crate) content_type_info: OnceLock<MediaInfo>,
    /// Lazily-built fragmented-mp4 segment index. Populated on first
    /// segment-method call once the file is fully cached and parses
    /// as fragmented mp4. Stays empty for non-mp4 files, classic mp4
    /// (no `moof` chain), or while the file is still downloading.
    pub(crate) segment_index: OnceLock<FileSegmentIndex>,

    /// FSM phase as `FilePhase as u8`. Lock-free transitions.
    phase: AtomicU8,
}

impl FileInner {
    pub(crate) fn new(
        source: FileSourceCtx,
        asset: FileAssetCtx,
        initial_phase: FilePhase,
    ) -> Self {
        let inner = Self {
            source,
            asset,
            content_type_info: OnceLock::new(),
            segment_index: OnceLock::new(),
            phase: AtomicU8::new(initial_phase as u8),
        };
        if matches!(initial_phase, FilePhase::Complete) {
            inner.try_build_segment_index();
        }
        inner
    }

    /// Mark the resource failed and evict the pre-allocated cache file.
    ///
    /// Call on any terminal download error so the file is gone from disk
    /// before the task returns — without this the clone in `FileInner.asset.res`
    /// keeps the mmap parked in the cache directory for the full lifetime
    /// of the holding `Stream<File>`.
    pub(crate) fn fail_and_evict(&self, reason: &str) {
        self.set_phase(FilePhase::Complete);
        self.asset.res.fail(reason.to_string());
        self.asset.backend.remove_resource(&self.asset.key);
    }

    /// Lock-free FSM transition. The one-shot fragmented-mp4 parse runs
    /// on the `Complete` edge so the hot-path `as_segment_layout` audit
    /// can short-circuit on `segment_index.get()` without re-reading the
    /// file each tick.
    pub(crate) fn set_phase(&self, phase: FilePhase) {
        self.phase.store(phase as u8, Ordering::Release);
        if matches!(phase, FilePhase::Complete) {
            self.try_build_segment_index();
        }
    }

    /// One-shot fragmented-mp4 parse from the fully cached file bytes.
    /// Idempotent: a second call is a `OnceLock::get` fast-path no-op.
    /// Called from `set_phase(Complete)` (and from `new` for files
    /// constructed already-complete) so the hot-path `as_segment_layout`
    /// audit only ever reads the cached result.
    fn try_build_segment_index(&self) {
        if self.segment_index.get().is_some() {
            return;
        }
        let Some(total) = self.asset.res.len() else {
            return;
        };
        if total == 0 || !self.asset.res.contains_range(0..total) {
            return;
        }
        let Ok(total_usize) = usize::try_from(total) else {
            return;
        };
        let mut buf: Box<[u8]> = std::iter::repeat_n(0u8, total_usize).collect();
        if self.asset.res.read_at(0, &mut buf).is_err() {
            return;
        }
        if let Some(index) = FileSegmentIndex::try_build(&buf) {
            let _ = self.segment_index.set(index);
        }
    }
}
