#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_abr::{AbrDecision, AbrState};
use kithara_assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::AbrReason;
use kithara_hls::{
    HlsCoord, HlsSource, PlaylistState, SegmentData, SegmentLoader, StreamIndex, test_utils,
};
use kithara_platform::{Mutex, time::Instant};
use kithara_storage::ResourceExt;
use tokio_util::sync::CancellationToken;

/// Pin the ABR state to a fixed variant via the documented production
/// write path (`AbrState::apply` with a `ManualOverride` decision). Same
/// effect as the controller applying a manual mode change, without needing
/// a controller tick.
pub fn pin_abr_variant(abr: &AbrState, idx: usize) {
    abr.apply(
        &AbrDecision {
            reason: AbrReason::ManualOverride,
            did_change: true,
            target_variant_index: idx,
        },
        Instant::now(),
    );
}

/// Build an ephemeral asset store with a passthrough DRM processor —
/// suitable as a backend for integration tests that don't care about
/// real encryption.
#[must_use]
pub fn make_test_backend(cancel: &CancellationToken) -> AssetStore<DecryptContext> {
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

/// Build a `(backend, SegmentLoader)` pair sharing the supplied cancel
/// token. Delegates to `kithara_hls::test_utils::loader_pair` to keep the
/// internal `HlsPeer` plumbing inside `kithara-hls`.
#[must_use]
pub fn make_test_loader(
    cancel: &CancellationToken,
) -> (AssetStore<DecryptContext>, Arc<SegmentLoader>) {
    test_utils::loader_pair(cancel)
}

/// Alias of [`make_test_loader`] preserved for tests that want the
/// "segment loader" framing in their fixture name.
#[must_use]
pub fn make_test_segment_loader(
    cancel: &CancellationToken,
) -> (AssetStore<DecryptContext>, Arc<SegmentLoader>) {
    make_test_loader(cancel)
}

/// Construct an `HlsSource` over a fresh ephemeral backend.
#[must_use]
pub fn make_test_source(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    cancel: &CancellationToken,
) -> HlsSource {
    let (backend, _loader) = make_test_loader(cancel);
    test_utils::build_source(coord, playlist_state, segments, backend)
}

/// Construct an `HlsSource` over a caller-supplied backend.
#[must_use]
pub fn make_test_source_with_backend(
    playlist_state: Arc<PlaylistState>,
    segments: Arc<Mutex<StreamIndex>>,
    coord: Arc<HlsCoord>,
    backend: AssetStore<DecryptContext>,
) -> HlsSource {
    test_utils::build_source(coord, playlist_state, segments, backend)
}

/// Commit a dummy resource (zero-filled body) for the media URL recorded
/// in `data`, plus the matching init resource if the segment carries
/// one. Tests use this to satisfy `Source::wait_range` against a
/// stripped fixture that doesn't actually run the loader.
///
/// # Panics
///
/// Panics if any of the resource-store operations fail; the helper is
/// only intended for tests, where panics bubble up as test failures
/// with the underlying storage error message.
pub fn commit_dummy_resource_from_data(source: &HlsSource, data: &SegmentData) {
    let backend = source.backend();
    let media_key = ResourceKey::from_url(&data.media_url);
    let res = backend
        .acquire_resource(&media_key)
        .expect("open media resource");
    let media_len = usize::try_from(data.media_len).unwrap_or(usize::MAX);
    res.write_at(0, &vec![0u8; media_len]).expect("write media");
    res.commit(None).expect("commit media");

    if let Some(ref init_url) = data.init_url {
        let init_key = ResourceKey::from_url(init_url);
        let init_res = backend
            .acquire_resource(&init_key)
            .expect("open init resource");
        let init_len = usize::try_from(data.init_len).unwrap_or(usize::MAX);
        init_res
            .write_at(0, &vec![0u8; init_len])
            .expect("write init");
        init_res.commit(None).expect("commit init");
    }
}
