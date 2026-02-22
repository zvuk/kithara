#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize},
    },
};

pub use kithara_abr::{AbrMode, AbrOptions};
use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
use kithara_coverage::CoverageManager;
use kithara_drm::DecryptContext;
use kithara_events::{Event, EventBus};
use kithara_net::{HttpClient, NetOptions};
use kithara_storage::StorageResource;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::source::build_pair;
pub use crate::{
    config::HlsConfig,
    download_state::{DownloadState, LoadedSegment},
    error::HlsError,
    fetch::{DefaultFetchManager, FetchManager},
    keys::KeyManager,
    parsing::{
        MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
        parse_media_playlist, variant_info_from_master,
    },
    playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
    source::{HlsSource, SegmentRequest, SharedSegments},
};

fn make_test_fetch(cancel: CancellationToken) -> Arc<DefaultFetchManager> {
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
    let net = HttpClient::new(NetOptions::default());
    Arc::new(FetchManager::new(backend, net, cancel))
}

/// Build a test-friendly `HlsSource` with an in-memory backend.
///
/// # Panics
/// Panics when the coverage manager cannot be opened from the ephemeral backend.
pub fn make_test_source(shared: Arc<SharedSegments>, cancel: CancellationToken) -> HlsSource {
    let fetch = make_test_fetch(cancel);
    let coverage = fetch
        .backend()
        .open_coverage_manager()
        .expect("coverage manager should open");
    let playlist_state = Arc::clone(&shared.playlist_state);
    HlsSource {
        fetch,
        shared,
        playlist_state,
        bus: EventBus::new(16),
        coverage,
        variant_fence: None,
        _backend: None,
    }
}

#[must_use]
pub fn build_source(
    fetch: Arc<DefaultFetchManager>,
    variants: &[VariantStream],
    config: &HlsConfig,
    coverage: CoverageManager<StorageResource>,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
) -> HlsSource {
    let (_downloader, source) = build_pair(fetch, variants, config, coverage, playlist_state, bus);
    source
}

pub fn set_source_coverage(source: &mut HlsSource, coverage: CoverageManager<StorageResource>) {
    source.coverage = coverage;
}

#[must_use]
pub fn source_coverage(source: &HlsSource) -> CoverageManager<StorageResource> {
    source.coverage.clone()
}

pub fn set_source_variant_fence(source: &mut HlsSource, fence: Option<usize>) {
    source.variant_fence = fence;
}

#[must_use]
pub fn subscribe_source_events(source: &HlsSource) -> broadcast::Receiver<Event> {
    source.bus.subscribe()
}

#[must_use]
pub fn source_can_cross_variant(
    source: &HlsSource,
    from_variant: usize,
    to_variant: usize,
) -> bool {
    source.can_cross_variant_without_reset(from_variant, to_variant)
}

#[must_use]
pub fn source_range_ready_from_segments(
    source: &HlsSource,
    segments: &DownloadState,
    range: &Range<u64>,
) -> bool {
    source.range_ready_from_segments(segments, range)
}

#[must_use]
pub fn source_segment_index_handle(source: &HlsSource) -> Arc<AtomicU32> {
    source.segment_index_handle()
}

#[must_use]
pub fn source_variant_index_handle(source: &HlsSource) -> Arc<AtomicUsize> {
    source.variant_index_handle()
}
