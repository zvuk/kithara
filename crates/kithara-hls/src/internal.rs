#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicUsize},
};

pub use kithara_abr::{AbrMode, AbrOptions};
use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
use kithara_drm::DecryptContext;
use kithara_events::{Event, EventBus};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::source::build_pair;
pub use crate::{
    config::HlsConfig,
    coord::{HlsCoord, SegmentRequest},
    error::HlsError,
    fetch::{DefaultFetchManager, FetchManager},
    keys::KeyManager,
    parsing::{
        MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
        parse_media_playlist, variant_info_from_master,
    },
    playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
    source::HlsSource,
    stream_index::{SegmentData, StreamIndex},
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

pub fn make_test_source(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    cancel: CancellationToken,
) -> HlsSource {
    let fetch = make_test_fetch(cancel);
    make_test_source_with_fetch(playlist_state, segments, coord, fetch)
}

pub fn make_test_source_with_fetch(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    fetch: Arc<DefaultFetchManager>,
) -> HlsSource {
    HlsSource {
        coord,
        fetch,
        segments,
        playlist_state,
        bus: EventBus::new(16),
        variant_fence: None,
        _backend: None,
    }
}

/// Create test fetch manager (public for tests that need shared fetch).
#[must_use]
pub fn make_test_fetch_manager(cancel: CancellationToken) -> Arc<DefaultFetchManager> {
    make_test_fetch(cancel)
}

/// Commit dummy resource from a `SegmentData` reference.
#[expect(
    clippy::cast_possible_truncation,
    reason = "test helper — segment lengths fit in usize"
)]
#[expect(clippy::missing_panics_doc, reason = "test-only helper")]
pub fn commit_dummy_resource_from_data(source: &HlsSource, data: &SegmentData) {
    use kithara_assets::ResourceKey;
    use kithara_storage::ResourceExt;

    let backend = source.fetch.backend();
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
    fetch: Arc<DefaultFetchManager>,
    variants: &[VariantStream],
    config: &HlsConfig,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
) -> HlsSource {
    let (_downloader, source) = build_pair(fetch, variants, config, playlist_state, bus);
    source
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
    segments: &StreamIndex,
    range: &Range<u64>,
) -> bool {
    source.range_ready_from_segments(segments, range)
}

#[must_use]
pub fn source_variant_index_handle(source: &HlsSource) -> Arc<AtomicUsize> {
    Arc::clone(&source.coord.abr_variant_index)
}
