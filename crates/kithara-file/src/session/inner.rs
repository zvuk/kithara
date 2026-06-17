use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU8, Ordering},
};

use kithara_assets::{
    AssetReader, AssetResource, AssetStore, AssetWriter, DemandLease, RawWriteHandle, ReadSide,
    ResourceKey, WriteSide,
};
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Mutex};
use kithara_stream::MediaInfo;
use url::Url;

use super::segments::FileSegmentIndex;
use crate::{coord::FileCoord, error::SourceError};

/// Creation helper: acquire the asset resource and prepare the event bus
/// before constructing a remote `FileSource`. The phase-typed acquisition
/// (`Pending` writer to download / `Ready` reader already cached) is decided
/// by the caller.
pub(crate) struct FileStreamState {
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) acq: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) key: ResourceKey,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &Arc<AssetStore>,
        key: ResourceKey,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
    ) -> Result<Self, SourceError> {
        let acq = assets
            .acquire_resource(&key, None)
            .map_err(SourceError::Assets)?;
        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));
        Ok(Self {
            bus,
            key,
            acq,
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
    pub(crate) cancel: CancelToken,
    pub(crate) bus: EventBus,
}

/// Data-plane handles describing where the file lives and how to fetch it.
///
/// The resource is phase-split: `reader` serves the synchronous read path;
/// `writer` is the single non-`Clone` commit owner (taken on commit/fail),
/// present only on the download path; `raw` is the clone-able streaming-write
/// handle a fetch closure uses to land bytes into the writer's generation.
pub(crate) struct FileAssetCtx {
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) reader: AssetReader,
    pub(crate) writer: Mutex<Option<AssetWriter>>,
    pub(crate) headers: Option<Headers>,
    pub(crate) raw: Option<RawWriteHandle>,
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

    /// Consumer demand lease held for this source's lifetime. `Some` on
    /// the remote-download path: it keeps the demand slot alive and lets
    /// [`FilePeer`](super::FilePeer) take over the single-producer
    /// election if the original producer drops. `None` for local /
    /// already-cached sources that never download.
    pub(crate) demand_lease: Option<DemandLease>,

    /// FSM phase as `FilePhase as u8`. Lock-free transitions.
    phase: AtomicU8,
}

impl FileInner {
    pub(crate) fn new(
        source: FileSourceCtx,
        asset: FileAssetCtx,
        initial_phase: FilePhase,
        demand_lease: Option<DemandLease>,
    ) -> Self {
        let inner = Self {
            source,
            asset,
            demand_lease,
            content_type_info: OnceLock::new(),
            segment_index: OnceLock::new(),
            phase: AtomicU8::new(initial_phase as u8),
        };
        if matches!(initial_phase, FilePhase::Complete) {
            inner.try_build_segment_index();
        }
        inner
    }

    /// Read the fully cached file bytes and parse a fragmented-mp4 index,
    /// or `None` when the file is not yet complete or not fragmented mp4.
    fn build_segment_index_from_cache(&self) -> Option<FileSegmentIndex> {
        let total = self.asset.reader.len()?;
        if total == 0 || !self.asset.reader.contains_range(0..total) {
            return None;
        }
        let total_usize = usize::try_from(total).ok()?;
        let mut buf: Box<[u8]> = std::iter::repeat_n(0u8, total_usize).collect();
        self.asset.reader.read_at(0, &mut buf).ok()?;
        FileSegmentIndex::try_build(&buf)
    }

    /// Mark the resource failed and evict the pre-allocated cache file.
    ///
    /// Call on any terminal download error so the file is gone from disk
    /// before the task returns — without this the reader in `FileInner.asset`
    /// keeps the mmap parked in the cache directory for the full lifetime
    /// of the holding `Stream<File>`.
    pub(crate) fn fail_and_evict(&self, reason: &str) {
        self.set_phase(FilePhase::Complete);
        if let Some(writer) = self.take_writer() {
            writer.fail(reason.to_string());
        }
        self.asset.backend.remove_resource(&self.asset.key);
    }

    /// Lock-free FSM transition. The one-shot fragmented-mp4 parse runs
    /// on the `Complete` edge so the hot-path `byte_map` audit
    /// can short-circuit on `segment_index.get()` without re-reading the
    /// file each tick.
    pub(crate) fn set_phase(&self, phase: FilePhase) {
        self.phase.store(phase as u8, Ordering::Release);
        if matches!(phase, FilePhase::Complete) {
            self.try_build_segment_index();
        }
    }

    /// Take the single commit-owning writer, if this is a download path and it
    /// has not been consumed yet.
    pub(crate) fn take_writer(&self) -> Option<AssetWriter> {
        self.asset.writer.lock().take()
    }

    /// One-shot fragmented-mp4 parse from the fully cached file bytes.
    /// Idempotent: a second call is a `OnceLock::get` fast-path no-op.
    /// Called from `set_phase(Complete)` (and from `new` for files
    /// constructed already-complete) so the hot-path `byte_map`
    /// audit only ever reads the cached result.
    fn try_build_segment_index(&self) {
        if self.segment_index.get().is_some() {
            return;
        }
        if let Some(index) = self.build_segment_index_from_cache() {
            let _ = self.segment_index.set(index);
        }
    }
}
