use std::{
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, atomic::Ordering},
    thread,
    time::Duration as StdDuration,
};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_platform::{
    time::Duration,
    tokio::time::{Duration as TokioDuration, timeout},
};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{ReadOutcome, Source, SourcePhase, SourceSeekAnchor, StreamError};
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

fn test_peer_handle(cancel: &CancellationToken) -> kithara_stream::dl::PeerHandle {
    let dl = kithara_stream::dl::Downloader::new(
        kithara_stream::dl::DownloaderConfig::default().with_cancel(cancel.child_token()),
    );
    dl.register(Arc::new(crate::peer::HlsPeer::new()))
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
    (0..count)
        .map(|index| VariantStream {
            id: VariantId(index),
            uri: format!("v{index}.m3u8"),
            bandwidth: Some(128_000),
            name: None,
            codec: None,
        })
        .collect()
}

fn make_variant_state_with_segments(id: usize, segments: usize) -> VariantState {
    let base = Url::parse("https://example.com/").expect("valid base URL");
    VariantState {
        id,
        uri: base
            .join(&format!("v{id}.m3u8"))
            .expect("valid playlist URL"),
        bandwidth: Some(128_000),
        codec: None,
        container: None,
        init_url: None,
        segments: (0..segments)
            .map(|index| SegmentState {
                index,
                url: base
                    .join(&format!("seg-{id}-{index}.m4s"))
                    .expect("valid segment URL"),
                duration: Duration::from_secs(4),
                key: None,
            })
            .collect(),
        size_map: None,
    }
}

fn build_test_source_with_segments(num_variants: usize, segments_per_variant: usize) -> HlsSource {
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
        EventBus::new(16),
    );
    source
}

fn build_test_source(num_variants: usize) -> HlsSource {
    build_test_source_with_segments(num_variants, 1)
}

fn build_source_with_size_map(segment_sizes: &[u64]) -> HlsSource {
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
        EventBus::new(16),
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
    let source = build_test_source(1);
    push_segment(&source.segments, 0, 0, 0, 100);
    push_segment(&source.segments, 0, 1, 100, 100);

    let anchor = make_anchor(0, 0, 0);
    let layout = source.classify_seek(&anchor);
    assert!(matches!(layout, SeekLayout::Preserve));
}

#[kithara::test]
fn seek_resolves_in_layout_variant_not_abr_target() {
    let source = build_test_source_with_segments(2, 4);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[500, 500, 500, 500]);
    push_segment(&source.segments, 0, 0, 0, 100);

    // ABR wants variant 1, but layout is variant 0
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = source
        .resolve_seek_anchor(Duration::from_millis(500))
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
    let mut source = build_test_source_with_segments(2, 4);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
    push_segment(&source.segments, 0, 0, 0, 100);

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
    let source = build_test_source_with_segments(2, 3);
    // Commit segments to layout variant (0)
    push_segment(&source.segments, 0, 0, 0, 100);
    push_segment(&source.segments, 0, 1, 100, 100);
    push_segment(&source.segments, 0, 2, 200, 100);

    source.coord.timeline().set_byte_position(250);

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
    let mut source = build_test_source_with_segments(2, 2);
    push_segment(&source.segments, 0, 0, 0, 100);
    {
        let mut segments = source.segments.lock_sync();
        segments.set_layout_variant(1);
    }
    push_segment(&source.segments, 1, 1, 100, 100);
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
    let source = build_test_source(2);
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
    let mut source = build_test_source_with_segments(2, 4);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
    // ABR wants variant 1, but seek must use layout_variant (0)
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
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
    let mut source = build_test_source_with_segments(2, 4);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[500, 500, 500, 500]);
    push_segment(&source.segments, 0, 0, 0, 100);
    push_segment(&source.segments, 0, 1, 100, 100);

    // ABR switches to variant 1 mid-stream
    source.coord.abr_variant_index.store(1, Ordering::Release);
    source
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);

    // Seek resolves in layout_variant (0), ignoring ABR
    let anchor = source
        .resolve_seek_anchor(Duration::from_millis(4_000))
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
    let mut source = build_test_source(2);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100]);

    // Layout variant = 0, commit and evict segment 0
    push_segment(&source.segments, 0, 0, 0, 100);
    push_segment(&source.segments, 0, 1, 100, 100);

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
    let mut source = build_test_source_with_segments(2, 4);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100, 100, 100]);
    source.coord.abr_variant_index.store(1, Ordering::Release);

    let anchor = make_anchor(1, 2, 200);
    source.apply_seek_plan(&anchor, &SeekLayout::Reset);
    source.coord.timeline().set_byte_position(150);

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
    let source = build_test_source(1);
    let wake = source.coord.reader_advanced.notified();

    let queued = source.push_segment_request(0, 0, 7);
    assert!(queued, "push_segment_request must succeed");

    let req = source
        .coord
        .take_segment_request()
        .expect("request must be queued");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 0);
    assert_eq!(req.seek_epoch, 7);

    timeout(TokioDuration::from_millis(10), wake)
        .await
        .expect("initial on-demand request must wake downloader");
}

#[kithara::test]
fn on_demand_request_returns_false_when_dedupe_suppresses_enqueue() {
    let source = build_test_source(1);
    let request = SegmentRequest {
        variant: 0,
        segment_index: 0,
        seek_epoch: 7,
    };
    source.coord.enqueue_segment_request(request);

    let queued = source.push_segment_request(0, 0, 7);

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
    let source = build_test_source(2);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100]);
    set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100]);

    // Layout variant = 0, commit and evict segment 0
    push_segment(&source.segments, 0, 0, 0, 100);
    push_segment(&source.segments, 0, 1, 100, 100);

    let evicted = ResourceKey::from_url(
        &Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
    );
    let removed = source.segments.lock_sync().remove_resource(&evicted);
    assert!(removed);

    // ABR wants variant 1, but layout is still variant 0
    source.coord.abr_variant_index.store(1, Ordering::Release);

    assert!(
        source.queue_segment_request_for_offset(0, 7),
        "hole at offset 0 must queue a recovery request"
    );
    assert_eq!(
        source.coord.take_segment_request(),
        Some(SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: 7,
        }),
        "recovery must target the layout variant that owns the missing segment"
    );
}

#[kithara::test]
fn wait_range_reissues_request_after_pending_request_is_cleared() {
    let mut source = build_source_with_size_map(&[100]);
    source.coord.stopped.store(false, Ordering::Release);
    let request = SegmentRequest {
        variant: 0,
        segment_index: 0,
        seek_epoch: 0,
    };
    source.coord.enqueue_segment_request(request);

    let coord = Arc::clone(&source.coord);
    let join = thread::spawn(move || {
        thread::sleep(StdDuration::from_millis(10));
        coord.clear_pending_segment_request(request);
        coord.condvar.notify_all();
    });

    let result = source.wait_range(0..1, Duration::from_millis(120));
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
    let mut source = build_source_with_size_map(&[100, 100, 100]);
    source.coord.stopped.store(false, Ordering::Release);

    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: 2,
        seek_epoch: 0,
    });

    let result = source.wait_range(0..1, Duration::from_millis(80));

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
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, 3,
    )]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes: vec![100, 100, 100],
            offsets: vec![0, 100, 200],
            total: 300,
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
        EventBus::new(16),
    );

    source.demand_range(150..151);

    let req = source
        .coord
        .take_segment_request()
        .expect("demand_range must queue an on-demand segment request");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 1);
}

#[kithara::test]
fn demand_range_does_not_queue_last_segment_at_exact_total_bytes() {
    let source = build_source_with_size_map(&[100, 100, 100]);

    source.demand_range(300..301);
    assert!(
        source.coord.take_segment_request().is_none(),
        "offset at exact total_bytes must not fallback to the last segment"
    );
}

#[kithara::test]
fn format_change_segment_range_prefers_metadata_for_stale_init_segment_offset() {
    // Need 3 segments so StreamIndex variant_map covers segment 1
    let cancel = CancellationToken::new();
    let variant = make_variant_state_with_segments(0, 3);
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
        EventBus::new(16),
    );

    source.playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes: vec![100, 100, 100],
            offsets: vec![0, 100, 200],
            total: 300,
        },
    );

    source.segments.lock_sync().commit_segment(
        0,
        1,
        SegmentData {
            init_len: 25,
            media_len: 75,
            init_url: None,
            media_url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid segment URL"),
        },
    );

    // Segment 1 has init but segment 0 is not committed yet.
    // Must return segment 0's metadata range (0..100) so the decoder
    // starts at offset 0 and sees the full stream.
    assert_eq!(source.format_change_segment_range(), Some(0..100));
}

#[kithara::test]
fn format_change_segment_range_uses_layout_floor_when_no_segments_are_loaded() {
    let source = build_source_with_size_map(&[100, 100, 100, 100]);

    // With per-variant byte maps, layout_floor is always segment 0.
    // No segments committed → fallback to metadata for segment 0.
    assert_eq!(
        source.format_change_segment_range(),
        Some(0..100),
        "without committed segments, decoder-start fallback uses metadata for layout variant segment 0"
    );
}

#[kithara::test]
fn layout_variant_preserves_total_length() {
    let source = build_source_with_size_map(&[100, 100, 100, 100]);

    assert_eq!(
        source.len(),
        Some(400),
        "total length must reflect all segments in the layout variant"
    );
}

#[kithara::test]
fn set_seek_epoch_drains_pending_segment_requests() {
    let mut source = build_test_source(1);
    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: 1,
        seek_epoch: 3,
    });
    source.coord.enqueue_segment_request(SegmentRequest {
        variant: 0,
        segment_index: 2,
        seek_epoch: 4,
    });

    source.set_seek_epoch(9);

    assert!(
        source.coord.take_segment_request().is_none(),
        "set_seek_epoch must drain pending segment requests"
    );
}

#[kithara::test]
fn set_seek_epoch_keeps_exact_eof_visible_until_seek_lands() {
    let mut source = build_source_with_size_map(&[100, 100, 100]);
    source.coord.stopped.store(false, Ordering::Release);
    source.coord.timeline().set_byte_position(300);
    source.coord.timeline().set_eof(true);

    source.set_seek_epoch(7);

    let result = source.wait_range(300..301, Duration::from_millis(50));

    assert!(
        matches!(result, Ok(WaitOutcome::Eof)),
        "exact EOF must stay observable during the seek-reset window"
    );
}

#[kithara::test]
fn wait_range_uses_known_total_bytes_for_exact_eof() {
    let mut source = build_source_with_size_map(&[100, 100, 100]);
    source.coord.stopped.store(false, Ordering::Release);
    source.coord.timeline().set_byte_position(300);
    source.coord.timeline().set_eof(false);

    let result = source.wait_range(300..301, Duration::from_millis(50));

    assert!(
        matches!(result, Ok(WaitOutcome::Eof)),
        "exact EOF must not depend on a separately refreshed eof flag when total_bytes is known"
    );
}

#[kithara::test]
fn read_media_segment_checked_reads_active_resource_in_ephemeral_mode() {
    let source = build_test_source(1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    media_res.write_at(0, b"media_data").unwrap();

    let seg = ReadSegment {
        variant: 0,
        segment_index: 0,
        byte_offset: 0,
        init_len: 0,
        media_len: 10,
        init_url: None,
        media_url,
    };
    let mut buf = [0u8; 10];

    let read = source
        .read_media_segment_checked(&seg, 0, &mut buf)
        .unwrap();

    assert_eq!(read, Some(10));
    assert_eq!(&buf, b"media_data");
}

#[kithara::test]
fn read_at_does_not_advance_timeline_position() {
    let mut source = build_test_source(1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    media_res.write_at(0, b"media_data").unwrap();
    media_res.commit(Some(10)).unwrap();

    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: 10,
            init_url: None,
            media_url,
        },
    );
    source.coord.timeline().set_byte_position(0);

    let mut buf = [0u8; 10];
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
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, 3,
    )]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            segment_sizes: vec![100, 100, 100],
            offsets: vec![0, 100, 200],
            total: 300,
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
        EventBus::new(16),
    );

    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
        },
    );

    let mut buf = [0u8; 32];
    let read = source.read_at(150, &mut buf).unwrap();

    assert_eq!(
        read,
        ReadOutcome::Retry,
        "layout hole before effective total must trigger retry instead of synthetic EOF"
    );
}

#[kithara::test]
fn wait_range_allows_short_read_when_range_crosses_known_eof() {
    let mut source = build_source_with_size_map(&[300]);
    source.coord.stopped.store(false, Ordering::Release);

    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = &source.backend.acquire_resource(&media_key).unwrap();
    media_res.write_at(0, &[0u8; 300]).unwrap();
    media_res.commit(Some(300)).unwrap();

    source.segments.lock_sync().commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: 300,
            init_url: None,
            media_url,
        },
    );

    let result = source.wait_range(250..350, Duration::from_millis(50));

    assert!(
        matches!(result, Ok(WaitOutcome::Ready)),
        "range that starts before EOF must be readable even if it extends past EOF"
    );
}

#[kithara::test]
fn read_at_disk_reopened_segments_return_committed_bytes_after_eviction() {
    let cancel = CancellationToken::new();
    let dir = tempdir().expect("temp dir");
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0, 4,
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
        EventBus::new(16),
    );

    let mut segments = Vec::new();
    for index in 0..4 {
        let media_url =
            Url::parse(&format!("https://example.com/seg-0-{index}.m4s")).expect("valid URL");
        let media_key = ResourceKey::from_url(&media_url);
        let payload: Vec<u8> = (0u8..=u8::MAX)
            .cycle()
            .skip(index * 13)
            .take(128 * 1024 + index * 1024 + 17)
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
    let source = build_test_source(num_variants);
    source.coord.stopped.store(false, Ordering::Release);
    source
}

#[kithara::test]
fn hls_phase_ready_when_range_ready() {
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, 100);

    // Write actual resource data so ephemeral backend reports it as present.
    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &[0u8; 100]).unwrap();
    res.commit(Some(100)).unwrap();

    assert_eq!(source.phase_at(0..50), SourcePhase::Ready);
}

#[kithara::test]
fn hls_phase_waiting_when_active_segment_does_not_cover_requested_range() {
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, 100);

    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &[0u8; 16]).unwrap();

    assert_eq!(source.phase_at(0..16), SourcePhase::Ready);
    assert_eq!(source.phase_at(0..50), SourcePhase::Waiting);
}

#[kithara::test]
fn hls_phase_seeking_when_flushing() {
    let source = build_phase_test_source(1);
    let _ = source
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(0));

    assert_eq!(source.phase_at(0..50), SourcePhase::Seeking);
}

#[kithara::test]
fn hls_phase_eof_when_past_effective_total() {
    let source = build_source_with_size_map(&[100]);
    source.coord.stopped.store(false, Ordering::Release);
    push_segment(&source.segments, 0, 0, 0, 100);
    source.coord.timeline().set_eof(true);

    assert_eq!(source.phase_at(200..250), SourcePhase::Eof);
}

#[kithara::test]
fn hls_phase_cancelled_when_cancel_token_set() {
    let source = build_test_source(1);
    source.coord.cancel.cancel();

    assert_eq!(source.phase_at(0..50), SourcePhase::Cancelled);
}

#[kithara::test]
fn hls_phase_cancelled_when_stopped() {
    let source = build_test_source(1);
    source.coord.stopped.store(true, Ordering::Release);

    assert_eq!(source.phase_at(0..50), SourcePhase::Cancelled);
}

#[kithara::test]
fn hls_phase_waiting_when_no_data() {
    let source = build_phase_test_source(1);

    assert_eq!(source.phase_at(0..50), SourcePhase::Waiting);
}

// Source::phase() parameterless override tests

#[kithara::test]
fn hls_phase_parameterless_ready_when_segment_loaded() {
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, 100);

    // Write actual resource data so ephemeral backend reports it as present.
    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &[0u8; 100]).unwrap();
    res.commit(Some(100)).unwrap();

    assert_eq!(source.phase(), SourcePhase::Ready);
}

#[kithara::test]
fn hls_phase_parameterless_waiting_when_segment_only_partially_streamed() {
    let source = build_phase_test_source(1);
    push_segment(&source.segments, 0, 0, 0, 100);

    let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
    let res = &source.backend.acquire_resource(&key).unwrap();
    res.write_at(0, &[0u8; 16]).unwrap();

    assert_eq!(source.phase(), SourcePhase::Waiting);
}

#[kithara::test]
fn hls_phase_parameterless_waiting_when_no_segments() {
    let source = build_phase_test_source(1);

    assert_eq!(source.phase(), SourcePhase::Waiting);
}

#[kithara::test]
fn queue_segment_request_resolves_offset_to_segment() {
    let source = build_test_source(1);
    source.coord.stopped.store(false, Ordering::Release);
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100]);

    // Offset 50 should resolve to segment 0, variant 0.
    let queued = source.queue_segment_request_for_offset(50, 0);
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
    let mut source = build_test_source_with_segments(1, 4);

    // DRM padding shrinks each segment: size_map totals 396 bytes.
    set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 99, 97]);

    // Anchor says seek target was segment 2 at byte offset 200.
    let anchor = make_anchor(0, 2, 200);
    source.apply_seek_plan(&anchor, &SeekLayout::Preserve);

    // Decoder lands at byte_position = 397 — past size_map.total (396).
    // Mirrors the production log: `landed_offset=27229732` with
    // find_segment_at_offset returning None because the FLAC size_map
    // shrank below that offset after DRM padding removal.
    source.coord.timeline().set_byte_position(397);

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
         set variant_fence when landed_offset ({}) is past the shrunk \
         size_map total; otherwise the reader stalls on wait_range",
        397,
    );
}

/// RED test #1 (integration: live_ephemeral_revisit_sequence_regression_drm_hw)
///
/// When the scheduler re-acquires a DRM resource (via `acquire_resource_with_ctx`)
/// for a previously-committed segment — e.g. after LRU eviction pressure —
/// the fresh `ProcessedResource` starts in `processed=false` state until the
/// new download commits. Meanwhile, the reader's `open_resource(key)` (ctx=None)
/// can end up holding that uncommitted entry, and `read_at` propagates
/// `StorageError::Failed("processed resource is not readable before commit")`
/// as a hard Err, poisoning the decoder FSM (`TrackState::Failed`). The reader
/// then sees `is_eof()==true` and the test panics with "stopped early at idx".
#[kithara::test]
fn red_test_drm_hw_read_at_returns_retry_when_drm_resource_reacquired_uncommitted() {
    use kithara_stream::ReadOutcome;

    let mut source = build_test_source_with_segments(1, 1);
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL");
    let media_key = ResourceKey::from_url(&media_url);
    let payload = vec![0xABu8; 4096];

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
    let mut buf = vec![0u8; 64];
    let first = source.read_at(0, &mut buf).expect("first read_at");
    assert_eq!(first, ReadOutcome::Data(64));

    // Step 2: simulate scheduler-driven re-fetch — a new DRM acquire
    // displaces the committed cached resource with a fresh uncommitted one
    // (mirrors the cache_capacity pressure in the ephemeral DRM scenario).
    let _fresh = source
        .backend
        .acquire_resource_with_ctx(&media_key, Some(DecryptContext::default()))
        .expect("reacquire fresh uncommitted");

    // Step 3: the reader's next read_at. Since the fresh ProcessedResource
    // is uncommitted (`processed=false`), reading it returns
    // StorageError::Failed("... not readable before commit"), which source
    // propagates as hard Err. The expected behaviour is `ReadOutcome::Retry`.
    let mut buf2 = vec![0u8; 64];
    let outcome = source
        .read_at(0, &mut buf2)
        .expect("read_at must not return hard error for uncommitted DRM reacquire");
    assert_eq!(
        outcome,
        ReadOutcome::Retry,
        "expected Retry when the DRM resource was reacquired and not yet \
         committed; got {outcome:?}. Current code returns Err → decoder FSM \
         goes Failed → is_eof()==true → test sees 'stopped early'"
    );
}
