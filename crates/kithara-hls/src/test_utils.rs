use std::sync::{Arc, atomic::AtomicUsize};

use kithara_assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn};
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_platform::Mutex;
use kithara_stream::{
    Timeline,
    dl::{Downloader, DownloaderConfig, Peer},
};
use tokio_util::sync::CancellationToken;

use crate::{
    HlsConfig, HlsCoord, HlsSource, PlaylistState, SegmentLoader, StreamIndex, VariantStream,
    loading::PlaylistCache, peer::HlsPeer, scheduler::HlsScheduler, source::HlsSegmentView,
};

/// Build a `(backend, loader)` pair on a fresh `Downloader`. Mirrors the
/// dispatch tree a real `Stream<Hls>` builds, scoped to the supplied
/// cancel token so tests release resources on drop.
#[must_use]
pub fn loader_pair(cancel: &CancellationToken) -> (AssetStore<DecryptContext>, Arc<SegmentLoader>) {
    let passthrough: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .cancel(cancel.clone())
        .process_fn(passthrough)
        .build();
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));
    let handle = downloader.register(Arc::new(HlsPeer::new(
        Timeline::new(),
        kithara_abr::AbrMode::default(),
    )));
    let cache = PlaylistCache::new(
        backend.clone(),
        handle.clone(),
        kithara_bufpool::BytePool::default(),
    );
    let loader = Arc::new(SegmentLoader::new(handle, backend.clone(), None, cache));
    (backend, loader)
}

/// Construct an `HlsSource` from raw deps. For tests that need a source
/// without a full `Stream<Hls>` setup.
#[must_use]
pub fn build_source(
    coord: Arc<HlsCoord>,
    playlist_state: Arc<PlaylistState>,
    segments: Arc<Mutex<StreamIndex>>,
    backend: AssetStore<DecryptContext>,
) -> HlsSource {
    let segmented_view = HlsSegmentView::new(Arc::clone(&playlist_state), Arc::clone(&segments));
    HlsSource {
        coord,
        backend,
        segments,
        playlist_state,
        segmented_view,
        bus: EventBus::new(kithara_events::DEFAULT_EVENT_BUS_CAPACITY),
        variant_fence: None,
        _hls_peer: None,
        _peer_handle: None,
        reader_segment: Arc::new(AtomicUsize::new(0)),
    }
}

/// Build an `HlsSource` from configuration plus variants. Mirrors the
/// production path that goes through `HlsScheduler::spawn_source`
/// without requiring callers to instantiate a `Stream<Hls>` first.
#[must_use]
pub fn build_source_full(
    backend: AssetStore<DecryptContext>,
    variants: &[VariantStream],
    config: &HlsConfig,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
) -> HlsSource {
    let downloader = config
        .downloader
        .clone()
        .unwrap_or_else(|| Downloader::new(DownloaderConfig::default()));
    let timeline = Timeline::new();
    let hls_peer = Arc::new(HlsPeer::new(timeline.clone(), config.initial_abr_mode));
    let abr_variants: Vec<kithara_events::AbrVariant> = variants
        .iter()
        .map(|v| kithara_events::AbrVariant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
            duration: kithara_events::VariantDuration::Unknown,
        })
        .collect();
    hls_peer.set_abr_variants(abr_variants);
    let _handle = downloader.register(Arc::clone(&hls_peer) as Arc<dyn Peer>);
    let scheduler = HlsScheduler::new(
        backend,
        playlist_state,
        Arc::clone(hls_peer.abr()),
        timeline,
        bus,
        config,
        hls_peer.committed_segment_cursor(),
    );
    scheduler.spawn_source(hls_peer.reader_segment_cursor())
}
