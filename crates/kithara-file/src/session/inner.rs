use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};

use kithara_assets::{
    AssetReader, AssetStore, AssetWriter, DemandLease, RawWriteHandle, ReadSide,
    ResourceAcquisition, ResourceKey, WriteSide,
};
use kithara_events::{AudioCodecKind, ContainerKind, EventBus, FileEvent, TotalBytesSource};
use kithara_net::Headers;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
};
use kithara_stream::{MediaInfo, WorkerWake};
use url::Url;

use super::segments::FileSegmentIndex;
use crate::{coord::FileCoord, error::SourceError};

/// Creation helper: acquire the asset resource and prepare the event bus
/// before constructing a remote `FileSource`. The phase-typed acquisition
/// (`Pending` writer to download / `Ready` reader already cached) is decided
/// by the caller.
pub(crate) struct FileStreamState {
    pub(crate) backend: AssetStore,
    pub(crate) acq: ResourceAcquisition,
    pub(crate) bus: EventBus,
    pub(crate) key: ResourceKey,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &AssetStore,
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
            backend: assets.clone(),
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
    pub(crate) backend: AssetStore,
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
    opened_emitted: OnceLock<()>,

    /// FSM phase as `FilePhase as u8`. Lock-free transitions.
    phase: AtomicU8,

    /// Late-bound audio-worker wake. Remote file sources can underrun while
    /// HTTP bytes are still arriving, so each write wakes the worker that
    /// previously parked on a non-blocking readiness probe.
    worker_wake: OnceLock<Arc<dyn WorkerWake>>,
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
            opened_emitted: OnceLock::new(),
            content_type_info: OnceLock::new(),
            segment_index: OnceLock::new(),
            worker_wake: OnceLock::new(),
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
        if let Some(writer) = self.take_writer() {
            writer.fail(reason.to_string());
        }
        if let Err(error) = self.asset.backend.remove_resource(&self.asset.key) {
            tracing::warn!(%error, reason, "kithara-file: failed to remove terminal resource");
        }
    }

    /// Lock-free FSM transition. The one-shot fragmented-mp4 parse runs
    /// on the `Complete` edge so the hot-path `byte_map` audit
    /// can short-circuit on `segment_index.get()` without re-reading the
    /// file each tick.
    pub(crate) fn set_phase(&self, phase: FilePhase) {
        let previous = self.phase.load(Ordering::Acquire);
        self.phase.store(phase as u8, Ordering::Release);
        if matches!(phase, FilePhase::Complete) && previous != FilePhase::Complete as u8 {
            if let Some(total_bytes) = self.source.coord.total_bytes() {
                self.source
                    .bus
                    .publish(FileEvent::CacheComplete { total_bytes });
            }
            self.try_build_segment_index();
        }
    }

    pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        let _ = self.worker_wake.set(wake);
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

    pub(crate) fn wake_worker(&self) {
        if let Some(wake) = self.worker_wake.get() {
            wake.wake();
        }
    }

    pub(crate) fn publish_opened(
        &self,
        total_bytes: Option<u64>,
        cached: bool,
        source: Option<TotalBytesSource>,
    ) {
        if self.opened_emitted.set(()).is_err() {
            return;
        }
        let (codec, container) = self
            .content_type_info
            .get()
            .map_or((None, None), map_media_info);
        self.source.bus.publish(FileEvent::Opened {
            codec,
            container,
            total_bytes,
            cached,
        });
        if let (Some(total_bytes), Some(source)) = (total_bytes, source) {
            self.source.bus.publish(FileEvent::TotalBytesResolved {
                total_bytes,
                source,
            });
        }
    }

    pub(crate) fn publish_total_bytes_resolved(&self, total_bytes: u64, source: TotalBytesSource) {
        self.source.bus.publish(FileEvent::TotalBytesResolved {
            total_bytes,
            source,
        });
    }
}

fn map_media_info(info: &MediaInfo) -> (Option<AudioCodecKind>, Option<ContainerKind>) {
    (
        info.codec.map(map_audio_codec),
        info.container.map(map_container),
    )
}

fn map_audio_codec(codec: kithara_stream::AudioCodec) -> AudioCodecKind {
    match codec {
        kithara_stream::AudioCodec::AacLc => AudioCodecKind::AacLc,
        kithara_stream::AudioCodec::AacHe => AudioCodecKind::AacHe,
        kithara_stream::AudioCodec::AacHeV2 => AudioCodecKind::AacHeV2,
        kithara_stream::AudioCodec::Mp3 => AudioCodecKind::Mp3,
        kithara_stream::AudioCodec::Flac => AudioCodecKind::Flac,
        kithara_stream::AudioCodec::Vorbis => AudioCodecKind::Vorbis,
        kithara_stream::AudioCodec::Opus => AudioCodecKind::Opus,
        kithara_stream::AudioCodec::Alac => AudioCodecKind::Alac,
        kithara_stream::AudioCodec::Pcm => AudioCodecKind::Pcm,
        kithara_stream::AudioCodec::Adpcm => AudioCodecKind::Adpcm,
    }
}

fn map_container(container: kithara_stream::ContainerFormat) -> ContainerKind {
    match container {
        kithara_stream::ContainerFormat::Mp4 => ContainerKind::Mp4,
        kithara_stream::ContainerFormat::Fmp4 => ContainerKind::Fmp4,
        kithara_stream::ContainerFormat::MpegTs => ContainerKind::MpegTs,
        kithara_stream::ContainerFormat::MpegAudio => ContainerKind::MpegAudio,
        kithara_stream::ContainerFormat::Adts => ContainerKind::Adts,
        kithara_stream::ContainerFormat::Flac => ContainerKind::Flac,
        kithara_stream::ContainerFormat::Wav => ContainerKind::Wav,
        kithara_stream::ContainerFormat::Ogg => ContainerKind::Ogg,
        kithara_stream::ContainerFormat::Caf => ContainerKind::Caf,
        kithara_stream::ContainerFormat::Mkv => ContainerKind::Mkv,
    }
}
