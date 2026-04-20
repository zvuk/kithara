use std::{
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, atomic::Ordering},
    thread,
    time::Duration as StdDuration,
};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{Event, EventBus, HlsEvent};
use kithara_platform::{
    time::Duration,
    tokio::{
        sync::broadcast::error::TryRecvError,
        time::{Duration as TokioDuration, timeout},
    },
};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    ReadOutcome, Source, SourcePhase, SourceSeekAnchor, StreamError,
    dl::{Downloader, DownloaderConfig, PeerHandle},
};
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    core::{HlsSource, build_pair},
    types::{ReadSegment, SeekLayout},
};
use crate::{
    HlsError,
    config::HlsConfig,
    coord::SegmentRequest,
    parsing::{VariantId, VariantStream},
    playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
    stream_index::{SegmentData, StreamIndex},
};

fn test_peer_handle(cancel: &CancellationToken) -> PeerHandle {
    let dl = Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));
    dl.register(Arc::new(crate::peer::HlsPeer::new(
        kithara_stream::Timeline::new(),
    )))
}

type LoaderPair = (
    kithara_assets::AssetStore<DecryptContext>,
    Arc<crate::loading::SegmentLoader>,
);

fn make_test_loader_pair(
    cancel: CancellationToken,
    backend: kithara_assets::AssetStore<DecryptContext>,
) -> LoaderPair {
    let handle = test_peer_handle(&cancel);
    let cache = crate::loading::PlaylistCache::new(backend.clone(), handle.clone());
    let loader = Arc::new(crate::loading::SegmentLoader::new(
        handle,
        backend.clone(),
        None,
        cache,
    ));
    (backend, loader)
}

fn test_fetch_manager(cancel: CancellationToken) -> LoaderPair {
    let noop_drm: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .cancel(cancel.clone())
        .process_fn(noop_drm)
        .build();
    make_test_loader_pair(cancel, backend)
}

fn test_disk_fetch_manager(cancel: CancellationToken, root_dir: &Path) -> LoaderPair {
    let noop_drm: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = AssetStoreBuilder::new()
        .root_dir(root_dir)
        .asset_root(Some("source-disk-test"))
        .cache_capacity(NonZeroUsize::new(1).expect("non-zero"))
        .cancel(cancel.clone())
        .process_fn(noop_drm)
        .build();
    make_test_loader_pair(cancel, backend)
}

fn parsed_variants(count: usize) -> Vec<VariantStream> {
    const BANDWIDTH: u64 = 128_000;
    (0..count)
        .map(|index| VariantStream {
            id: VariantId(index),
            uri: format!("v{index}.m3u8"),
            bandwidth: Some(BANDWIDTH),
            name: None,
            codec: None,
        })
        .collect()
}

fn make_variant_state_with_segments(id: usize, segments: usize) -> VariantState {
    const BANDWIDTH: u64 = 128_000;
    const SEGMENT_SECS: u64 = 4;
    let base = Url::parse("https://example.com/").expect("valid base URL");
    VariantState {
        id,
        uri: base
            .join(&format!("v{id}.m3u8"))
            .expect("valid playlist URL"),
        bandwidth: Some(BANDWIDTH),
        codec: None,
        container: None,
        init_url: None,
        segments: (0..segments)
            .map(|index| SegmentState {
                index,
                url: base
                    .join(&format!("seg-{id}-{index}.m4s"))
                    .expect("valid segment URL"),
                duration: Duration::from_secs(SEGMENT_SECS),
                key: None,
            })
            .collect(),
        size_map: None,
    }
}

fn build_test_source_with_segments(num_variants: usize, segments_per_variant: usize) -> HlsSource {
    const BUS_CAPACITY: usize = 16;
    let cancel = CancellationToken::new();
    let variants: Vec<VariantState> = (0..num_variants)
        .map(|index| make_variant_state_with_segments(index, segments_per_variant))
        .collect();
    let playlist_state = Arc::new(PlaylistState::new(variants));
    let parsed = parsed_variants(num_variants);
    let (backend, _loader) = test_fetch_manager(cancel.clone());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );
    source
}

fn build_source_with_size_map(segment_sizes: &[u64]) -> HlsSource {
    const BUS_CAPACITY: usize = 16;
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        segment_sizes.len(),
    )]));
    let mut total = 0;
    let offsets: Vec<u64> = segment_sizes
        .iter()
        .map(|size| {
            let offset = total;
            total += size;
            offset
        })
        .collect();
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes: segment_sizes.to_vec(),
            offsets,
            total,
        },
    );
    let parsed = parsed_variants(1);
    let (backend, _loader) = test_fetch_manager(cancel.clone());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );
    source
}

fn set_variant_size_map(state: &PlaylistState, variant: usize, segment_sizes: &[u64]) {
    let mut total = 0;
    let offsets: Vec<u64> = segment_sizes
        .iter()
        .map(|size| {
            let offset = total;
            total += size;
            offset
        })
        .collect();
    state.set_size_map(
        variant,
        VariantSizeMap {
            segment_sizes: segment_sizes.to_vec(),
            offsets,
            total,
        },
    );
}

fn push_segment(
    segments: &Arc<kithara_platform::Mutex<StreamIndex>>,
    variant: usize,
    index: usize,
    _offset: u64,
    len: u64,
) {
    let data = SegmentData {
        init_len: 0,
        media_len: len,
        init_url: None,
        media_url: Url::parse(&format!("https://example.com/seg-{variant}-{index}.m4s"))
            .expect("valid segment URL"),
    };
    segments.lock_sync().commit_segment(variant, index, data);
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "test helper, segment index fits u32"
)]
fn make_anchor(variant: usize, segment: usize, offset: u64) -> SourceSeekAnchor {
    SourceSeekAnchor::new(offset, Duration::ZERO)
        .with_variant_index(variant)
        .with_segment_index(segment as u32)
}

#[kithara::test]
fn same_variant_seek_preserves_segments() {
    const SEG_LEN: u64 = 100;
    let source = build_test_source_with_segments(1, 1);
    push_segment(&source.segments, 0, 0, 0, SEG_LEN);
    push_segment(&source.segments, 0, 1, SEG_LEN, SEG_LEN);

    let anchor = make_anchor(0, 0, 0);
    let layout = source.classify_seek(&anchor);
    assert!(matches!(layout, SeekLayout::Preserve));
}

#[kithara::test]
fn seek_resolves_in_layout_variant_not_abr_target() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 4;
    const V0_SEG_SIZE: u64 = 100;
    const V1_SEG_SIZE: u64 = 500;
    const SEG0_LEN: u64 = 100;
    const SEEK_MS: u64 = 500;
    let source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(
        source.playlist_state.as_ref(),
        0,
        &[V0_SEG_SIZE, V0_SEG_SIZE, V0_SEG_SIZE, V0_SEG_SIZE],
    );
    set_variant_size_map(
        source.playlist_state.as_ref(),
        1,
        &[V1_SEG_SIZE, V1_SEG_SIZE, V1_SEG_SIZE, V1_SEG_SIZE],
    );
    push_segment(&source.segments, 0, 0, 0, SEG0_LEN);

    // ABR wants variant 1, but layout is variant 0
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = source
        .resolve_seek_anchor(Duration::from_millis(SEEK_MS))
        .expect("anchor resolution");

    // Anchor must use layout variant (0), NOT ABR target (1)
    assert_eq!(
        anchor.variant_index,
        Some(0),
        "seek must resolve in layout_variant, ignoring ABR target"
    );
}

#[kithara::test]
fn seek_does_not_switch_layout_variant() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 4;
    const SEG_SIZE: u64 = 100;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(
        source.playlist_state.as_ref(),
        0,
        &[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE],
    );
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);

    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = make_anchor(0, 0, 0);
    let layout = source.classify_seek(&anchor);
    source.apply_seek_plan(&anchor, &layout);

    assert_eq!(
        source.segments.lock_sync().layout_variant(),
        0,
        "seek must not switch layout_variant — ABR switch happens via format_change"
    );
}

#[kithara::test]
fn seek_within_layout_variant_preserves_segments() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 3;
    const SEG_SIZE: u64 = 100;
    const SEG2_INDEX: usize = 2;
    const SEG2_OFFSET: u64 = SEG_SIZE * 2;
    const MID_SEG_OFFSET: u64 = SEG_SIZE * 2 + SEG_SIZE / 2;
    let source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    // Commit segments to layout variant (0)
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    push_segment(&source.segments, 0, 1, SEG_SIZE, SEG_SIZE);
    push_segment(&source.segments, 0, SEG2_INDEX, SEG2_OFFSET, SEG_SIZE);

    source.coord.timeline().set_byte_position(MID_SEG_OFFSET);

    // Seek to variant 0 (same as layout_variant) → Preserve
    let anchor = make_anchor(0, 0, 0);
    let layout = source.classify_seek(&anchor);
    assert!(
        matches!(layout, SeekLayout::Preserve),
        "seek within the layout variant must preserve StreamIndex"
    );
}

#[kithara::test]
fn commit_seek_landing_keeps_switched_tail_in_mixed_layout() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 2;
    const SEG_SIZE: u64 = 100;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    {
        let mut segments = source.segments.lock_sync();
        segments.set_layout_variant(1);
    }
    push_segment(&source.segments, 1, 1, SEG_SIZE, SEG_SIZE);
    source.coord.timeline().set_byte_position(0);

    source.commit_seek_landing(Some(make_anchor(0, 0, 0)));

    assert_eq!(
        source.segments.lock_sync().layout_variant(),
        1,
        "landing on the prefix must not rewrite the switched tail back to variant 0"
    );
}

#[kithara::test]
fn resolve_current_variant_uses_layout_variant() {
    const NUM_VARIANTS: usize = 2;
    let source = build_test_source_with_segments(NUM_VARIANTS, 1);
    source.coord.abr_variant_index.store(1, Ordering::Release);

    // resolve_current_variant uses layout_variant, not ABR
    assert_eq!(
        source.resolve_current_variant(),
        0,
        "current variant must come from layout_variant, not ABR atomic"
    );
}

#[kithara::test]
fn seek_anchor_uses_layout_variant_not_abr_target() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 4;
    const SEG_SIZE: u64 = 100;
    const SEEK_MS: u64 = 8_500;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(
        source.playlist_state.as_ref(),
        0,
        &[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE],
    );
    // Layout variant has the target segment committed, so the anchor
    // must stay on layout (no need to pivot to ABR) — this is the
    // "in-place seek" happy path.
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    push_segment(&source.segments, 0, 1, SEG_SIZE, SEG_SIZE);
    push_segment(&source.segments, 0, 2, 2 * SEG_SIZE, SEG_SIZE);
    // ABR wants variant 1, but seek must use layout_variant (0)
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(SEEK_MS))
        .expect("seek anchor resolution should not error")
        .expect("HLS source should resolve an anchor");

    assert_eq!(
        anchor.variant_index,
        Some(0),
        "seek must resolve in layout_variant (0), not ABR target (1)"
    );
    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(
        anchor.byte_offset, 200,
        "seek offset must be in layout variant's byte space"
    );
}

#[kithara::test]
fn abr_does_not_affect_any_seek_state() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 4;
    const V0_SEG_SIZE: u64 = 100;
    const V1_SEG_SIZE: u64 = 500;
    const SEEK_MS: u64 = 4_000;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(
        source.playlist_state.as_ref(),
        0,
        &[V0_SEG_SIZE, V0_SEG_SIZE, V0_SEG_SIZE, V0_SEG_SIZE],
    );
    set_variant_size_map(
        source.playlist_state.as_ref(),
        1,
        &[V1_SEG_SIZE, V1_SEG_SIZE, V1_SEG_SIZE, V1_SEG_SIZE],
    );
    push_segment(&source.segments, 0, 0, 0, V0_SEG_SIZE);
    push_segment(&source.segments, 0, 1, V0_SEG_SIZE, V0_SEG_SIZE);

    // ABR switches to variant 1 mid-stream
    source.coord.abr_variant_index.store(1, Ordering::Release);
    source
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);

    // Seek resolves in layout_variant (0), ignoring ABR
    let anchor = source
        .resolve_seek_anchor(Duration::from_millis(SEEK_MS))
        .expect("anchor");
    assert_eq!(anchor.variant_index, Some(0), "anchor ignores ABR");

    // classify_seek returns Preserve (same layout variant)
    let layout = source.classify_seek(&anchor);
    assert!(
        matches!(layout, SeekLayout::Preserve),
        "seek within layout_variant is always Preserve"
    );

    // apply_seek_plan does not change layout_variant
    source.apply_seek_plan(&anchor, &layout);
    assert_eq!(
        source.segments.lock_sync().layout_variant(),
        0,
        "layout_variant unchanged after seek"
    );

    // demand routing uses layout_variant (0), not ABR (1)
    assert_eq!(
        source.resolve_current_variant(),
        0,
        "demand variant is layout_variant during seek"
    );
}

#[kithara::test]
fn commit_seek_landing_uses_layout_variant_for_invalidated_segment() {
    const NUM_VARIANTS: usize = 2;
    const SEG_SIZE: u64 = 100;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, 1);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[SEG_SIZE, SEG_SIZE]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[SEG_SIZE, SEG_SIZE]);

    // Layout variant = 0, commit and evict segment 0
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    push_segment(&source.segments, 0, 1, SEG_SIZE, SEG_SIZE);

    let evicted = ResourceKey::from_url(
        &Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
    );
    assert!(source.segments.lock_sync().remove_resource(&evicted));

    // ABR wants variant 1, but layout is still variant 0
    source.coord.abr_variant_index.store(1, Ordering::Release);
    source.coord.timeline().set_byte_position(0);

    source.commit_seek_landing(Some(make_anchor(0, 0, 0)));

    // Recovery must target layout variant (0), not ABR target (1)
    assert_eq!(
        source.variant_fence,
        Some(0),
        "seek landing must fence reads to the layout variant, not the ABR target"
    );
    assert_eq!(
        source.coord.take_segment_request(),
        Some(SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: 0,
        }),
        "seek landing recovery must target the layout variant that owns the missing segment"
    );
}

#[kithara::test]
fn commit_seek_landing_uses_anchor_variant_metadata_when_reset_truncates_prefix() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 4;
    const SEG_SIZE: u64 = 100;
    const ANCHOR_VARIANT: usize = 1;
    const ANCHOR_SEGMENT: usize = 2;
    const ANCHOR_OFFSET: u64 = 200;
    const BYTE_POS: u64 = 150;
    let mut source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(
        source.playlist_state.as_ref(),
        0,
        &[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE],
    );
    set_variant_size_map(
        source.playlist_state.as_ref(),
        1,
        &[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE],
    );
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = make_anchor(ANCHOR_VARIANT, ANCHOR_SEGMENT, ANCHOR_OFFSET);
    source.apply_seek_plan(&anchor, &SeekLayout::Reset);
    source.coord.timeline().set_byte_position(BYTE_POS);

    source.commit_seek_landing(Some(anchor));

    assert_eq!(
        source.coord.take_segment_request(),
        Some(SegmentRequest {
            variant: 1,
            segment_index: 1,
            seek_epoch: 0,
        }),
        "decoder landing before the anchor must resolve through target-variant metadata, not the truncated reset layout"
    );
}

#[kithara::test(tokio)]
async fn on_demand_request_notifies_downloader_once() {
    const SEEK_EPOCH: u64 = 7;
    const TIMEOUT_MS: u64 = 10;
    let source = build_test_source_with_segments(1, 1);
    let wake = source.coord.reader_advanced.notified();

    let queued = source.push_segment_request(0, 0, SEEK_EPOCH);
    assert!(queued, "push_segment_request must succeed");

    let req = source
        .coord
        .take_segment_request()
        .expect("request must be queued");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 0);
    assert_eq!(req.seek_epoch, SEEK_EPOCH);

    timeout(TokioDuration::from_millis(TIMEOUT_MS), wake)
        .await
        .expect("initial on-demand request must wake downloader");
}

#[kithara::test]
fn on_demand_request_returns_false_when_dedupe_suppresses_enqueue() {
    const SEEK_EPOCH: u64 = 7;
    let source = build_test_source_with_segments(1, 1);
    let request = SegmentRequest {
        variant: 0,
        segment_index: 0,
        seek_epoch: SEEK_EPOCH,
    };
    source.coord.enqueue_segment_request(request);

    let queued = source.push_segment_request(0, 0, SEEK_EPOCH);

    assert!(
        !queued,
        "push_segment_request must report dedupe when enqueue is suppressed"
    );
    assert_eq!(
        source.coord.take_segment_request(),
        Some(request),
        "dedupe must keep the original pending request instead of enqueuing a duplicate"
    );
}

#[kithara::test]
fn queue_segment_request_uses_layout_variant_for_invalidated_segment() {
    const NUM_VARIANTS: usize = 2;
    const SEG_SIZE: u64 = 100;
    const SEEK_EPOCH: u64 = 7;
    let source = build_test_source_with_segments(NUM_VARIANTS, 1);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[SEG_SIZE, SEG_SIZE]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[SEG_SIZE, SEG_SIZE]);

    // Layout variant = 0, commit and evict segment 0
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    push_segment(&source.segments, 0, 1, SEG_SIZE, SEG_SIZE);

    let evicted = ResourceKey::from_url(
        &Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
    );
    let removed = source.segments.lock_sync().remove_resource(&evicted);
    assert!(removed);

    // ABR wants variant 1, but layout is still variant 0
    source.coord.abr_variant_index.store(1, Ordering::Release);

    assert!(
        source.queue_segment_request_for_offset(0, SEEK_EPOCH),
        "hole at offset 0 must queue a recovery request"
    );
    assert_eq!(
        source.coord.take_segment_request(),
        Some(SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: SEEK_EPOCH,
        }),
        "recovery must target the layout variant that owns the missing segment"
    );
}

#[kithara::test]
fn wait_range_reissues_request_after_pending_request_is_cleared() {
    const SEG_SIZE: u64 = 100;
    const CLEAR_DELAY_MS: u64 = 10;
    const TIMEOUT_MS: u64 = 120;
    let mut source = build_source_with_size_map(&[SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);
    let request = SegmentRequest {
        variant: 0,
        segment_index: 0,
        seek_epoch: 0,
    };
    source.coord.enqueue_segment_request(request);

    let coord = Arc::clone(&source.coord);
    let join = thread::spawn(move || {
        thread::sleep(StdDuration::from_millis(CLEAR_DELAY_MS));
        coord.clear_pending_segment_request(request);
        coord.condvar.notify_all();
    });

    let result = source.wait_range(0..1, Duration::from_millis(TIMEOUT_MS));
    join.join()
        .expect("clear-pending helper thread must complete");

    assert!(
        matches!(result, Err(StreamError::Source(HlsError::Timeout(_)))),
        "without a downloader the wait should still end by timeout"
    );

    let queued = source
        .coord
        .take_segment_request()
        .expect("wait_range must requeue the request once pending state clears");
    assert_eq!(queued, request);
}

#[kithara::test]
fn wait_range_replaces_mismatched_pending_request_for_same_epoch() {
    const SEG_SIZE: u64 = 100;
    const TIMEOUT_MS: u64 = 80;
    const STALE_SEG_INDEX: usize = 2;
    let mut source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);

    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: STALE_SEG_INDEX,
        seek_epoch: 0,
    });

    let result = source.wait_range(0..1, Duration::from_millis(TIMEOUT_MS));

    assert!(
        matches!(result, Err(StreamError::Source(HlsError::Timeout(_)))),
        "without a downloader the wait should still end by timeout"
    );

    let queued = source
        .coord
        .take_segment_request()
        .expect("wait_range must replace stale demand with the current segment request");
    assert_eq!(
        queued,
        SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: 0,
        }
    );
}

#[kithara::test]
fn demand_range_queues_request_for_unloaded_offset() {
    const NUM_SEGS: usize = 3;
    const SEG_SIZE: u64 = 100;
    const DEMAND_OFFSET: u64 = 150;
    const BUS_CAPACITY: usize = 16;
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, NUM_SEGS,
    )]));
    let offsets: Vec<u64> = (0..NUM_SEGS as u64).map(|i| i * SEG_SIZE).collect();
    let total = NUM_SEGS as u64 * SEG_SIZE;
    let segment_sizes: Vec<u64> = (0..NUM_SEGS).map(|_| SEG_SIZE).collect();
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes,
            offsets,
            total,
        },
    );
    let parsed = parsed_variants(1);
    let (backend, _loader) = test_fetch_manager(cancel.clone());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );

    source.demand_range(DEMAND_OFFSET..DEMAND_OFFSET + 1);

    let req = source
        .coord
        .take_segment_request()
        .expect("demand_range must queue an on-demand segment request");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 1);
}

#[kithara::test]
fn demand_range_does_not_queue_last_segment_at_exact_total_bytes() {
    const SEG_SIZE: u64 = 100;
    const NUM_SEGS: u64 = 3;
    let source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE]);
    let total = SEG_SIZE * NUM_SEGS;

    source.demand_range(total..total + 1);
    assert!(
        source.coord.take_segment_request().is_none(),
        "offset at exact total_bytes must not fallback to the last segment"
    );
}

#[kithara::test]
fn format_change_segment_range_prefers_metadata_for_stale_init_segment_offset() {
    const NUM_SEGS: usize = 3;
    const SEG_SIZE: u64 = 100;
    const INIT_LEN: u64 = 25;
    const MEDIA_LEN: u64 = 75;
    const BUS_CAPACITY: usize = 16;
    // Need 3 segments so StreamIndex variant_map covers segment 1
    let cancel = CancellationToken::new();
    let variant = make_variant_state_with_segments(0, NUM_SEGS);
    let playlist_state = Arc::new(PlaylistState::new(vec![variant]));
    let parsed = parsed_variants(1);
    let (backend, _loader) = test_fetch_manager(cancel.clone());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );

    let offsets: Vec<u64> = (0..NUM_SEGS as u64).map(|i| i * SEG_SIZE).collect();
    let total = NUM_SEGS as u64 * SEG_SIZE;
    let segment_sizes: Vec<u64> = (0..NUM_SEGS).map(|_| SEG_SIZE).collect();
    source.playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes,
            offsets,
            total,
        },
    );

    source.segments.lock_sync().commit_segment(
        0,
        1,
        SegmentData {
            init_len: INIT_LEN,
            media_len: MEDIA_LEN,
            init_url: None,
            media_url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid segment URL"),
        },
    );

    // Segment 1 has init but segment 0 is not committed yet.
    // Must return segment 0's metadata range (0..seg_size) so the decoder
    // starts at offset 0 and sees the full stream.
    assert_eq!(source.format_change_segment_range(), Some(0..SEG_SIZE));
}

#[kithara::test]
fn format_change_segment_range_uses_layout_floor_when_no_segments_are_loaded() {
    const SEG_SIZE: u64 = 100;
    let source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE]);

    // With per-variant byte maps, layout_floor is always segment 0.
    // No segments committed → fallback to metadata for segment 0.
    assert_eq!(
        source.format_change_segment_range(),
        Some(0..SEG_SIZE),
        "without committed segments, decoder-start fallback uses metadata for layout variant segment 0"
    );
}

#[kithara::test]
fn layout_variant_preserves_total_length() {
    const SEG_SIZE: u64 = 100;
    let source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE, SEG_SIZE]);

    assert_eq!(
        source.len(),
        Some(SEG_SIZE * 4),
        "total length must reflect all segments in the layout variant"
    );
}

#[kithara::test]
fn set_seek_epoch_drains_pending_segment_requests() {
    const EPOCH_A: u64 = 3;
    const EPOCH_B: u64 = 4;
    const NEW_EPOCH: u64 = 9;
    const SEG_INDEX_A: usize = 1;
    const SEG_INDEX_B: usize = 2;
    let mut source = build_test_source_with_segments(1, 1);
    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: SEG_INDEX_A,
        seek_epoch: EPOCH_A,
    });
    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: SEG_INDEX_B,
        seek_epoch: EPOCH_B,
    });

    source.set_seek_epoch(NEW_EPOCH);

    assert!(
        source.coord.take_segment_request().is_none(),
        "set_seek_epoch must drain pending segment requests"
    );
}

#[kithara::test]
fn set_seek_epoch_keeps_exact_eof_visible_until_seek_lands() {
    const SEG_SIZE: u64 = 100;
    const NUM_SEGS: u64 = 3;
    const NEW_EPOCH: u64 = 7;
    const TIMEOUT_MS: u64 = 50;
    let mut source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);
    let total = SEG_SIZE * NUM_SEGS;
    source.coord.timeline().set_byte_position(total);
    source.coord.timeline().set_eof(true);

    source.set_seek_epoch(NEW_EPOCH);

    let result = source.wait_range(total..total + 1, Duration::from_millis(TIMEOUT_MS));

    assert!(
        matches!(result, Ok(WaitOutcome::Eof)),
        "exact EOF must stay observable during the seek-reset window"
    );
}

#[kithara::test]
fn wait_range_uses_known_total_bytes_for_exact_eof() {
    const SEG_SIZE: u64 = 100;
    const NUM_SEGS: u64 = 3;
    const TIMEOUT_MS: u64 = 50;
    let mut source = build_source_with_size_map(&[SEG_SIZE, SEG_SIZE, SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);
    let total = SEG_SIZE * NUM_SEGS;
    source.coord.timeline().set_byte_position(total);
    source.coord.timeline().set_eof(false);

    let result = source.wait_range(total..total + 1, Duration::from_millis(TIMEOUT_MS));

    assert!(
        matches!(result, Ok(WaitOutcome::Eof)),
        "exact EOF must not depend on a separately refreshed eof flag when total_bytes is known"
    );
}

#[kithara::test]
fn read_media_segment_checked_reads_active_resource_in_ephemeral_mode() {
    let source = build_test_source_with_segments(1, 1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    media_res.write_at(0, b"media_data").unwrap();

    let media_len = b"media_data".len() as u64;
    let seg = ReadSegment {
        variant: 0,
        segment_index: 0,
        byte_offset: 0,
        init_len: 0,
        media_len,
        init_url: None,
        media_url,
    };
    let mut buf = vec![0u8; media_len as usize];

    let read = source
        .read_media_segment_checked(&seg, 0, &mut buf)
        .unwrap();

    assert_eq!(read, Some(10));
    assert_eq!(&buf, b"media_data");
}

#[kithara::test]
fn read_at_does_not_advance_timeline_position() {
    let mut source = build_test_source_with_segments(1, 1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    let media_data = b"media_data";
    media_res.write_at(0, media_data).unwrap();
    media_res.commit(Some(media_data.len() as u64)).unwrap();

    let media_len = media_data.len() as u64;
    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len,
            init_url: None,
            media_url,
        },
    );
    source.coord.timeline().set_byte_position(0);

    let mut buf = vec![0u8; media_len as usize];
    let read = source.read_at(0, &mut buf).unwrap();

    assert_eq!(read, ReadOutcome::Data(10));
    assert_eq!(
        source.coord.timeline().byte_position(),
        0,
        "HLS read_at must not commit the reader cursor outside Stream::read"
    );
}

#[kithara::test]
fn read_at_missing_segment_before_effective_total_returns_retry() {
    const NUM_SEGS: usize = 3;
    const SEG_SIZE: u64 = 100;
    const BUF_SIZE: usize = 32;
    const READ_OFFSET: u64 = 150;
    const BUS_CAPACITY: usize = 16;
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, NUM_SEGS,
    )]));
    let offsets: Vec<u64> = (0..NUM_SEGS as u64).map(|i| i * SEG_SIZE).collect();
    let total = NUM_SEGS as u64 * SEG_SIZE;
    let segment_sizes: Vec<u64> = (0..NUM_SEGS).map(|_| SEG_SIZE).collect();
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes,
            offsets,
            total,
        },
    );
    let parsed = parsed_variants(1);
    let (backend, _loader) = test_fetch_manager(cancel.clone());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, mut source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );

    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: SEG_SIZE,
            init_url: None,
            media_url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
        },
    );

    let mut buf = vec![0u8; BUF_SIZE];
    let read = source.read_at(READ_OFFSET, &mut buf).unwrap();

    assert_eq!(
        read,
        ReadOutcome::Retry,
        "layout hole before effective total must trigger retry instead of synthetic EOF"
    );
}

#[kithara::test]
fn wait_range_allows_short_read_when_range_crosses_known_eof() {
    const SEG_SIZE: u64 = 300;
    const RANGE_START: u64 = 250;
    const RANGE_END: u64 = 350;
    const TIMEOUT_MS: u64 = 50;
    let mut source = build_source_with_size_map(&[SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);

    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    media_res
        .write_at(0, &vec![0u8; SEG_SIZE as usize])
        .unwrap();
    media_res.commit(Some(SEG_SIZE)).unwrap();

    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: SEG_SIZE,
            init_url: None,
            media_url,
        },
    );

    let result = source.wait_range(RANGE_START..RANGE_END, Duration::from_millis(TIMEOUT_MS));

    assert!(
        matches!(result, Ok(WaitOutcome::Ready)),
        "range that starts before EOF must be readable even if it extends past EOF"
    );
}

#[kithara::test]
fn read_at_disk_reopened_segments_return_committed_bytes_after_eviction() {
    const NUM_SEGS: usize = 4;
    const SKIP_STRIDE: usize = 13;
    const BASE_KIB: usize = 128;
    const KIB_STRIDE: usize = 1024;
    const EXTRA_BYTES: usize = 17;
    const BUS_CAPACITY: usize = 16;
    let cancel = CancellationToken::new();
    let dir = tempdir().expect("temp dir");
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, NUM_SEGS,
    )]));
    let parsed = parsed_variants(1);
    let (backend, _loader) = test_disk_fetch_manager(cancel.clone(), dir.path());
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (_downloader, mut source) = build_pair(
        backend,
        track,
        &parsed,
        &config,
        playlist_state,
        EventBus::new(BUS_CAPACITY),
        kithara_stream::Timeline::new(),
    );

    let mut segments = Vec::new();
    for index in 0..NUM_SEGS {
        let media_url =
            Url::parse(&format!("https://example.com/seg-0-{index}.m4s")).expect("valid URL");
        let media_key = ResourceKey::from_url(&media_url);
        let payload: Vec<u8> = (0u8..=u8::MAX)
            .cycle()
            .skip(index * SKIP_STRIDE)
            .take(BASE_KIB * KIB_STRIDE + index * KIB_STRIDE + EXTRA_BYTES)
            .collect();
        let res = source
            .backend
            .acquire_resource(&media_key)
            .expect("open media resource");
        res.write_at(0, &payload).expect("write media");
        res.commit(Some(payload.len() as u64))
            .expect("commit media");
        source.segments.lock_sync().commit_segment(
            0,
            index,
            SegmentData {
                init_len: 0,
                media_len: payload.len() as u64,
                init_url: None,
                media_url,
            },
        );
        segments.push(payload);
    }

    let mut offset = 0u64;
    for payload in &segments {
        let mut buf = vec![0u8; payload.len()];
        let read = source.read_at(offset, &mut buf).expect("read_at");
        assert_eq!(read, ReadOutcome::Data(payload.len()));
        assert_eq!(buf, *payload);
        offset += payload.len() as u64;
    }
}

// Source::phase() trait method tests

/// Build source for phase tests — resets `stopped` flag that
/// `HlsScheduler::drop` sets when the downloader half is discarded.
fn build_phase_test_source(num_variants: usize) -> HlsSource {
    let source = build_test_source_with_segments(num_variants, 1);
    source.coord.stopped.store(false, Ordering::Release);
    source
}

#[kithara::test]
fn hls_phase_ready_when_range_ready() {
    const SEG_SIZE: u64 = 100;
    const RANGE_END: u64 = 50;
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);

    // Write actual resource data so ephemeral backend reports it as present.
    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &vec![0u8; SEG_SIZE as usize]).unwrap();
    res.commit(Some(SEG_SIZE)).unwrap();

    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Ready);
}

#[kithara::test]
fn hls_phase_waiting_when_active_segment_does_not_cover_requested_range() {
    const SEG_SIZE: u64 = 100;
    const WRITTEN: u64 = 16;
    const RANGE_END: u64 = 50;
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);

    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &vec![0u8; WRITTEN as usize]).unwrap();

    assert_eq!(source.phase_at(0..WRITTEN), SourcePhase::Ready);
    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Waiting);
}

#[kithara::test]
fn hls_phase_seeking_when_flushing() {
    const RANGE_END: u64 = 50;
    let source = build_phase_test_source(1);
    let _ = source
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(0));

    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Seeking);
}

#[kithara::test]
fn hls_phase_eof_when_past_effective_total() {
    const SEG_SIZE: u64 = 100;
    const RANGE_START: u64 = 200;
    const RANGE_END: u64 = 250;
    let source = build_source_with_size_map(&[SEG_SIZE]);
    source.coord.stopped.store(false, Ordering::Release);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    source.coord.timeline().set_eof(true);

    assert_eq!(source.phase_at(RANGE_START..RANGE_END), SourcePhase::Eof);
}

#[kithara::test]
fn hls_phase_cancelled_when_cancel_token_set() {
    const RANGE_END: u64 = 50;
    let source = build_test_source_with_segments(1, 1);
    source.coord.cancel.cancel();

    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Cancelled);
}

#[kithara::test]
fn hls_phase_cancelled_when_stopped() {
    const RANGE_END: u64 = 50;
    let source = build_test_source_with_segments(1, 1);
    source.coord.stopped.store(true, Ordering::Release);

    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Cancelled);
}

#[kithara::test]
fn hls_phase_waiting_when_no_data() {
    const RANGE_END: u64 = 50;
    let source = build_phase_test_source(1);

    assert_eq!(source.phase_at(0..RANGE_END), SourcePhase::Waiting);
}

// Source::phase() parameterless override tests

#[kithara::test]
fn hls_phase_parameterless_ready_when_segment_loaded() {
    const SEG_SIZE: u64 = 100;
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);

    // Write actual resource data so ephemeral backend reports it as present.
    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &vec![0u8; SEG_SIZE as usize]).unwrap();
    res.commit(Some(SEG_SIZE)).unwrap();

    assert_eq!(source.phase(), SourcePhase::Ready);
}

#[kithara::test]
fn hls_phase_parameterless_waiting_when_segment_only_partially_streamed() {
    const SEG_SIZE: u64 = 100;
    const WRITTEN: u64 = 16;
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);

    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &vec![0u8; WRITTEN as usize]).unwrap();

    assert_eq!(source.phase(), SourcePhase::Waiting);
}

#[kithara::test]
fn hls_phase_parameterless_waiting_when_no_segments() {
    let source = build_phase_test_source(1);

    assert_eq!(source.phase(), SourcePhase::Waiting);
}

#[kithara::test]
fn queue_segment_request_resolves_offset_to_segment() {
    const SEG_SIZE: u64 = 100;
    const QUERY_OFFSET: u64 = 50;
    let source = build_test_source_with_segments(1, 1);
    source.coord.stopped.store(false, Ordering::Release);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[SEG_SIZE, SEG_SIZE]);

    // Offset query_offset should resolve to segment 0, variant 0.
    let queued = source.queue_segment_request_for_offset(QUERY_OFFSET, 0);
    assert!(queued, "must queue a segment request for a valid offset");

    let req = source
        .coord
        .take_segment_request()
        .expect("request must be queued");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 0);
}

/// RED test #2 (integration: live_ephemeral_revisit_sequence_regression_drm_sw)
///
/// After DRM padding removal shrinks a variant's `size_map.total`, the
/// decoder can land at a `byte_position` that is >= the new total. Both
/// `layout_segment_for_offset` and `resolve_segment_for_offset` then
/// return `None`, and the current `commit_seek_landing` silently bails:
/// no `SegmentRequest` is enqueued, no `variant_fence` is set. The reader
/// then has no recovery path and `wait_range` loops until timeout.
#[kithara::test]
fn red_test_drm_sw_commit_seek_landing_enqueues_request_when_offset_past_total() {
    const NUM_SEGS: usize = 4;
    const S0: u64 = 100;
    const S1: u64 = 100;
    const S2: u64 = 99;
    const S3: u64 = 97;
    const ANCHOR_SEGMENT: usize = 2;
    const ANCHOR_OFFSET: u64 = 200;
    const LANDED_OFFSET: u64 = 397;
    let mut source = build_test_source_with_segments(1, NUM_SEGS);

    // DRM padding shrinks each segment: size_map totals s0+s1+s2+s3 bytes.
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[S0, S1, S2, S3]);

    // Anchor says seek target was anchor_segment at anchor_offset.
    let anchor = make_anchor(0, ANCHOR_SEGMENT, ANCHOR_OFFSET);
    source.apply_seek_plan(&anchor, &SeekLayout::Preserve);

    // Decoder lands at byte_position = landed_offset — past size_map.total.
    // Mirrors the production log: `landed_offset=27229732` with
    // find_segment_at_offset returning None because the FLAC size_map
    // shrank below that offset after DRM padding removal.
    source.coord.timeline().set_byte_position(LANDED_OFFSET);

    // Pre-condition: no segment requests are queued.
    assert_eq!(source.coord.take_segment_request(), None);

    source.commit_seek_landing(Some(anchor));

    // Either a concrete demand or a variant fence MUST be produced so
    // the reader has a recovery path. Today: both are empty → hang.
    let has_request = source.coord.take_segment_request().is_some();
    let has_fence = source.variant_fence.is_some();
    assert!(
        has_request || has_fence,
        "commit_seek_landing must enqueue a recovery SegmentRequest or \
         set variant_fence when landed_offset ({LANDED_OFFSET}) is past the shrunk \
         size_map total; otherwise the reader stalls on wait_range",
    );
}

/// Regression test (integration: live_ephemeral_revisit_sequence_regression_drm_hw)
///
/// When the scheduler re-acquires a DRM resource (via `acquire_resource_with_ctx`)
/// for a previously-committed segment — e.g. after LRU eviction pressure —
/// the already-cached committed resource must remain readable by any concurrent
/// reader. Previously `CachingAssets::acquire_resource_with_ctx` unconditionally
/// called `reactivate()` on a cache hit, which flipped `processed=false` on the
/// shared `ProcessedResource` and poisoned any reader holding a cloned Arc
/// (`read_at` fired "processed resource is not readable before commit" as a
/// hard `StorageError::Failed` → decoder FSM → Failed → `is_eof()==true` →
/// integration panic "stopped early at idx"). The fix: cache-hit on a
/// *Committed* resource must NOT reactivate — just return a clone. This
/// test pins that behaviour: the post-reacquire read still returns `Data`.
#[kithara::test]
fn read_at_serves_data_when_committed_drm_resource_is_reacquired() {
    const PAYLOAD_SIZE: usize = 4096;
    const READ_SIZE: usize = 64;
    let mut source = build_test_source_with_segments(1, 1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL");
    let media_key = ResourceKey::from_url(&media_url);
    let payload = vec![0xABu8; PAYLOAD_SIZE];

    // Step 1: acquire with DRM ctx, write, COMMIT → readable committed resource.
    {
        let res = source
            .backend
            .acquire_resource_with_ctx(&media_key, Some(DecryptContext::default()))
            .expect("acquire committed");
        res.write_at(0, &payload).expect("write");
        res.commit(Some(payload.len() as u64)).expect("commit");
    }
    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: payload.len() as u64,
            init_url: None,
            media_url: media_url.clone(),
        },
    );

    // Sanity: first read succeeds against the committed resource.
    let mut buf = vec![0u8; READ_SIZE];
    let first = source.read_at(0, &mut buf).expect("first read_at");
    assert_eq!(first, ReadOutcome::Data(READ_SIZE));

    // Step 2: simulate scheduler-driven re-fetch — a second DRM acquire
    // on the same key. Cache-hit branch must recognise the cached entry
    // is Committed and return a clone WITHOUT reactivating.
    let _fresh = source
        .backend
        .acquire_resource_with_ctx(&media_key, Some(DecryptContext::default()))
        .expect("reacquire of committed DRM resource");

    // Step 3: the reader's next read_at. The committed resource is still
    // readable, so `Data(read_size)` is the correct outcome. The old contract
    // (Retry) documented the bug itself — with `processed=false` flipped
    // by `reactivate()` the reader had to be told to try again.
    let mut buf2 = vec![0u8; READ_SIZE];
    let outcome = source
        .read_at(0, &mut buf2)
        .expect("read_at must not return hard error after reacquire");
    assert_eq!(
        outcome,
        ReadOutcome::Data(READ_SIZE),
        "expected Data({READ_SIZE}) after committed DRM reacquire — cache-hit must \
         not reactivate a Committed resource"
    );
}

/// RED test (integration: live_stress_real_stream_seek_read_cache_drm_mmap)
///
/// Symptom: under DRM + non-ephemeral mmap, the random-seek phase
/// (`RANDOM_PHASE_BUDGET_SECS = 5`, target ≥ `MIN_RANDOM_SEEKS = 50`)
/// regularly underflows ("stress seek underflow: expected at least 50
/// seek ops, got 29..48"). Profiling shows ~21k `apply_cached_segment_progress`
/// + `populate_cached_segments_if_needed` invocations per run when only
/// 37 cached segments exist on disk. Each `poll_next` pass re-emits one
/// `HlsEvent::SegmentComplete { cached: true, .. }` per *already-cached*
/// segment — there is no dedup of segments that have already been
/// announced, so the bus is flooded with N × poll_next events and the
/// listener task burns wall-clock time draining duplicates.
///
/// Contract under test: cached segments must be announced as
/// `SegmentComplete { cached: true }` *once* across repeated
/// `apply_cached_segment_progress` calls when the underlying
/// `populate_cached_segments_if_needed` finds nothing new. Otherwise
/// every poll_next on a steady state pays an O(N) event-publish cost,
/// starving the test's 5-second seek budget under contention.
///
/// Construction stays unit-scope: a non-ephemeral disk-backed
/// `AssetStore`, all 8 segments pre-committed on disk, then 5 back-to-back
/// `populate + apply_cached_segment_progress` cycles. With the bug, the
/// subscriber drains 8 × 5 = 40 cached-SegmentComplete events. With the
/// dedup invariant, it drains exactly 8.
#[kithara::test]
fn red_test_apply_cached_segment_progress_floods_events_on_repeat_polls() {
    const NUM_SEGMENTS: usize = 8;
    const SEGMENT_LEN: usize = 256;
    const POLL_CYCLES: usize = 5;
    const BUS_CAPACITY: usize = 1024;
    const BANDWIDTH: u64 = 128_000;
    const SEGMENT_SECS: u64 = 4;

    let dir = tempdir().expect("temp dir");
    let cancel = CancellationToken::new();

    // Non-ephemeral disk backend with a real on-disk asset_root so
    // populate_cached_segments_if_needed walks the disk path (not the
    // ephemeral early-return at size_map.rs:21-23).
    let noop_drm: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("flaky-mmap-red-test"))
        .cancel(cancel.clone())
        .process_fn(noop_drm)
        .build();
    assert!(
        !backend.is_ephemeral(),
        "RED test must hit the disk populate path, not the ephemeral early-return"
    );

    // Pre-commit every segment to disk so populate_cached_segments_if_needed
    // sees AssetResourceState::Committed for all of them on every call.
    let payload = vec![0xCDu8; SEGMENT_LEN];
    let base = Url::parse("https://example.com/").expect("base url");
    let mut segment_urls = Vec::with_capacity(NUM_SEGMENTS);
    for index in 0..NUM_SEGMENTS {
        let url = base.join(&format!("seg-0-{index}.m4s")).expect("seg url");
        let key = ResourceKey::from_url(&url);
        let res = backend
            .acquire_resource_with_ctx(&key, Some(DecryptContext::default()))
            .expect("acquire seg");
        res.write_at(0, &payload).expect("write seg");
        res.commit(Some(payload.len() as u64)).expect("commit seg");
        segment_urls.push(url);
    }

    // Build a matching playlist + scheduler.
    let variant = VariantState {
        id: 0,
        uri: base.join("v0.m3u8").expect("playlist url"),
        bandwidth: Some(BANDWIDTH),
        codec: None,
        container: None,
        init_url: None,
        segments: (0..NUM_SEGMENTS)
            .map(|index| SegmentState {
                index,
                url: segment_urls[index].clone(),
                duration: Duration::from_secs(SEGMENT_SECS),
                key: None,
            })
            .collect(),
        size_map: None,
    };
    let playlist_state = Arc::new(PlaylistState::new(vec![variant]));
    let parsed = parsed_variants(1);
    let track = test_peer_handle(&cancel);
    let config = HlsConfig {
        cancel: Some(cancel.clone()),
        ..HlsConfig::default()
    };
    let bus = EventBus::new(BUS_CAPACITY);
    let mut events = bus.subscribe();
    let (mut downloader, _source) = build_pair(
        backend.clone(),
        track,
        &parsed,
        &config,
        playlist_state,
        bus,
        kithara_stream::Timeline::new(),
    );

    // Drive POLL_CYCLES populate→apply cycles, mirroring what poll_next
    // does on every wake. Steady state: nothing new is committed
    // between cycles, yet the buggy code re-emits all NUM_SEGMENTS events.
    for _ in 0..POLL_CYCLES {
        let (cached_count, cached_end_offset) = downloader.populate_cached_segments_if_needed(0);
        downloader.apply_cached_segment_progress(0, cached_count, cached_end_offset);
    }

    let mut cached_announces = 0usize;
    loop {
        match events.try_recv() {
            Ok(Event::Hls(HlsEvent::SegmentComplete {
                cached: true,
                variant: 0,
                ..
            })) => cached_announces += 1,
            Ok(_) => {}
            Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
            Err(TryRecvError::Lagged(n)) => {
                panic!(
                    "broadcast receiver lagged by {n} events — the bus \
                     buffer overflowed because cached SegmentComplete \
                     events are being re-published every poll cycle, \
                     which is exactly the bug under test"
                );
            }
        }
    }

    assert_eq!(
        cached_announces,
        NUM_SEGMENTS,
        "expected each of the {NUM_SEGMENTS} cached segments to be \
         announced exactly once across {POLL_CYCLES} populate→apply \
         cycles, got {cached_announces}. Today \
         apply_cached_segment_progress republishes a SegmentComplete \
         event for every segment in 0..cached_count on every call, so \
         the count is NUM_SEGMENTS × POLL_CYCLES = {} — this O(N×polls) \
         flood explains the seek-underflow flakiness in \
         live_stress_real_stream_seek_read_cache_drm_mmap.",
        NUM_SEGMENTS * POLL_CYCLES,
    );
}

/// Regression guard for the ABR-stranded-layout seek hang.
///
/// Production scenario captured on silvercomet:
///   - Playback starts on variant 0, peer fetches segments 0..N0
///   - ABR decides to up-switch to variant 1; peer pivots to variant 1
///     segments and stops fetching variant 0
///   - Decoder is still mid-playback on variant 0 (format_change hasn't
///     fired yet — variant 0 data isn't exhausted)
///   - User seeks to a time past N0
///
/// With the old policy (anchor = layout_variant ignoring ABR), the
/// anchor resolves to variant 0 at a segment the downloader stopped
/// fetching. `source_is_ready_for_apply_seek` stays `Waiting` because
/// that byte range never arrives, and the track hangs silently.
///
/// The fix: when the layout variant has no committed segment at the
/// seek target, fall through to the variant ABR currently prefers —
/// which is exactly the variant the peer is actively downloading, so
/// the bytes the anchor points at will actually arrive.
#[kithara::test]
fn seek_anchor_falls_back_to_abr_when_layout_variant_has_no_target_segment() {
    const NUM_VARIANTS: usize = 2;
    const NUM_SEGS: usize = 6;
    const SEG_SIZE: u64 = 100;
    // Segments are 4 s each; 18 s lands squarely inside segment 4.
    const SEEK_MS: u64 = 18_000;
    let source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[SEG_SIZE; NUM_SEGS]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[SEG_SIZE; NUM_SEGS]);

    // Layout (variant 0) only captured the pre-switch prefix — segments
    // 0 and 1. Segment 4 (the seek target) is NOT present here.
    push_segment(&source.segments, 0, 0, 0, SEG_SIZE);
    push_segment(&source.segments, 0, 1, SEG_SIZE, SEG_SIZE);

    // ABR has since picked variant 1 and the peer is fetching it — even
    // if variant 1 has no committed segments yet, its forward fetch is
    // what will actually arrive at the anchor offset.
    source.coord.abr_variant_index.store(1, Ordering::Release);
    assert_eq!(
        source.segments.lock_sync().layout_variant(),
        0,
        "precondition: layout variant is still 0 (no recreation fired yet)"
    );

    let anchor = source
        .resolve_seek_anchor(Duration::from_millis(SEEK_MS))
        .expect("anchor resolution must succeed when any variant can cover the target");

    assert_eq!(
        anchor.variant_index,
        Some(1),
        "anchor must point at the ABR-preferred variant (the one the \
         peer is actively downloading); falling back to the stranded \
         layout variant keeps the track waiting on bytes that will \
         never arrive"
    );

    // Regression guard on the segment index too — if the fallback
    // resolved the time-to-segment lookup against the wrong variant,
    // it would land on a different segment than what the peer is
    // fetching.
    assert_eq!(
        anchor.segment_index,
        Some(4),
        "fallback anchor must resolve the segment index on the ABR \
         variant's timeline, not mix it with the layout variant"
    );
}

/// Reproduces the silvercomet seek hang from
/// `kithara_play::silvercomet_seek_hang`.
///
/// Scenario captured from a live trace:
///   - 3 AAC variants, playback starts on variant 0
///   - ABR up-switches to variant 2 (same codec) before the seek target
///     segment is committed to the layout variant
///   - `resolve_seek_anchor` correctly falls back to the ABR variant, so
///     `anchor.variant = 2` and `anchor.byte_offset` is in variant 2's
///     cumulative byte space
///   - Reader is still reading bytes from variant 0 so
///     `current_layout_variant()` returns 0
///
/// The bug: `classify_seek` sees `codec(0) == codec(2)` (both AAC) and
/// returns `Preserve`, so `apply_seek_plan` leaves the layout pinned at
/// variant 0. The decoder then seeks to an offset computed in variant 2's
/// byte space, but `wait_range` looks up that offset in variant 0's layout
/// where no segment covers it — `find_at_offset` returns `None` on every
/// poll and the track hangs for the full post-seek window (259 blocks of
/// silence in silvercomet).
///
/// Same-codec is not enough for `Preserve` across variants: byte-space is
/// per-variant and non-convertible. Cross-variant seek must always be
/// `Reset` so the layout is switched and the decoder is recreated on the
/// correct byte stream.
#[kithara::test]
fn cross_variant_seek_same_codec_requires_reset() {
    const NUM_VARIANTS: usize = 3;
    const NUM_SEGS: usize = 8;
    const V0_SEG_SIZE: u64 = 100;
    const V2_SEG_SIZE: u64 = 300;
    const READER_POS: u64 = V0_SEG_SIZE / 2;
    const TARGET_SEG_INDEX: usize = 5;
    const TARGET_OFFSET: u64 = V2_SEG_SIZE * TARGET_SEG_INDEX as u64;
    let source = build_test_source_with_segments(NUM_VARIANTS, NUM_SEGS);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[V0_SEG_SIZE; NUM_SEGS]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[V0_SEG_SIZE; NUM_SEGS]);
    set_variant_size_map(source.playlist_state.as_ref(), 2, &[V2_SEG_SIZE; NUM_SEGS]);

    // Reader sits inside variant 0's first segment → current layout = 0.
    push_segment(&source.segments, 0, 0, 0, V0_SEG_SIZE);
    source.coord.timeline().set_byte_position(READER_POS);
    assert_eq!(
        source.current_layout_variant(),
        Some(0),
        "precondition: reader is in variant 0's byte space"
    );

    // All three variants share the (None) codec — the production equivalent
    // is three AAC variants, which is what silvercomet serves.
    assert!(
        source.can_cross_variant_without_reset(0, 2),
        "precondition: variants 0 and 2 share a codec, so the old \
         Preserve policy would kick in"
    );

    // Anchor points at variant 2 (fallback after ABR up-switch stranded
    // the layout's target segment) and carries an offset in variant 2's
    // byte space.
    let anchor = make_anchor(2, TARGET_SEG_INDEX, TARGET_OFFSET);

    let layout = source.classify_seek(&anchor);
    assert!(
        matches!(layout, SeekLayout::Reset),
        "cross-variant seek must be Reset even when codecs match — \
         byte spaces are per-variant, Preserve would leave layout=0 \
         while the anchor offset lives in variant 2's space, hanging \
         wait_range forever; got {layout:?}"
    );
}
