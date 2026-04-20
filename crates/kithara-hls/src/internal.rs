#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicUsize},
};

pub use kithara_abr::{AbrMode, AbrOptions};
use kithara_assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{DEFAULT_EVENT_BUS_CAPACITY, EventBus};
use kithara_storage::ResourceExt;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tokio_util::sync::CancellationToken;

use crate::source::build_pair;
pub use crate::{
    config::HlsConfig,
    coord::{HlsCoord, SegmentRequest},
    error::HlsError,
    loading::{KeyManager, PlaylistCache, SegmentLoader},
    parsing::{
        MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
        parse_media_playlist, variant_info_from_master,
    },
    playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
    source::HlsSource,
    stream_index::{SegmentData, StreamIndex},
};

/// Build a test backend (ephemeral, passthrough DRM) and a private
/// downloader sharing the supplied cancel token.
fn make_test_backend(cancel: &CancellationToken) -> AssetStore<DecryptContext> {
    let passthrough: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    AssetStoreBuilder::new()
        .ephemeral(true)
        .cancel(cancel.clone())
        .process_fn(passthrough)
        .build()
}

fn make_test_loader(
    cancel: &CancellationToken,
) -> (AssetStore<DecryptContext>, Arc<SegmentLoader>) {
    let backend = make_test_backend(cancel);
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));
    let handle = downloader.register(Arc::new(crate::peer::HlsPeer::new(
        kithara_stream::Timeline::new(),
    )));
    let cache = PlaylistCache::new(backend.clone(), handle.clone());
    let loader = Arc::new(SegmentLoader::new(handle, backend.clone(), None, cache));
    (backend, loader)
}

pub fn make_test_source(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    cancel: &CancellationToken,
) -> HlsSource {
    let (backend, _loader) = make_test_loader(cancel);
    make_test_source_with_backend(playlist_state, segments, coord, backend)
}

pub fn make_test_source_with_backend(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    backend: AssetStore<DecryptContext>,
) -> HlsSource {
    HlsSource {
        coord,
        backend,
        segments,
        playlist_state,
        bus: EventBus::new(DEFAULT_EVENT_BUS_CAPACITY),
        variant_fence: None,
        _hls_peer: None,
        _peer_handle: None,
    }
}

/// Create a test segment loader + backend pair (public for tests that
/// need to drive a worker against an isolated backend).
#[must_use]
pub fn make_test_segment_loader(
    cancel: &CancellationToken,
) -> (AssetStore<DecryptContext>, Arc<SegmentLoader>) {
    make_test_loader(cancel)
}

/// Commit dummy resource from a `SegmentData` reference.
#[expect(
    clippy::cast_possible_truncation,
    reason = "test helper — segment lengths fit in usize"
)]
#[expect(clippy::missing_panics_doc, reason = "test-only helper")]
pub fn commit_dummy_resource_from_data(source: &HlsSource, data: &SegmentData) {
    let backend = &source.backend;
    let media_key = ResourceKey::from_url(&data.media_url);
    let res = backend
        .acquire_resource(&media_key)
        .expect("open media resource");
    res.write_at(0, &vec![0u8; data.media_len as usize])
        .expect("write media");
    res.commit(None).expect("commit media");

    if let Some(ref init_url) = data.init_url {
        let init_key = ResourceKey::from_url(init_url);
        let init_res = backend
            .acquire_resource(&init_key)
            .expect("open init resource");
        init_res
            .write_at(0, &vec![0u8; data.init_len as usize])
            .expect("write init");
        init_res.commit(None).expect("commit init");
    }
}

#[must_use]
pub fn build_source(
    backend: AssetStore<DecryptContext>,
    variants: &[VariantStream],
    config: &HlsConfig,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
) -> HlsSource {
    // Honour `HlsConfig::with_downloader`: reuse the supplied
    // [`Downloader`] when present, fall back to a private one for
    // tests that don't bother. Either way we create **one** per-track
    // handle here and hand it (cloned by `build_pair`) to all
    // sub-components — they share one track id and one cancellation
    // token.
    let downloader = config
        .downloader
        .clone()
        .unwrap_or_else(|| Downloader::new(DownloaderConfig::default()));
    let timeline = kithara_stream::Timeline::new();
    let handle = downloader.register(Arc::new(crate::peer::HlsPeer::new(timeline.clone())));
    let (_downloader, source) = build_pair(
        backend,
        handle,
        variants,
        config,
        playlist_state,
        bus,
        timeline,
    );
    source
}

pub fn set_source_variant_fence(source: &mut HlsSource, fence: Option<usize>) {
    source.variant_fence = fence;
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
    segments: &StreamIndex,
    range: &Range<u64>,
) -> bool {
    source.range_ready_from_segments(segments, range)
}

#[must_use]
pub fn source_variant_index_handle(source: &HlsSource) -> Arc<AtomicUsize> {
    Arc::clone(&source.coord.abr_variant_index)
}
