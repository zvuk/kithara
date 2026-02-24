use std::{
    ops::Range,
    slice,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
use kithara_coverage::{Coverage, CoverageManager};
use kithara_drm::DecryptContext;
use kithara_events::{Event, EventBus, HlsEvent};
use kithara_hls::internal::{
    AbrMode, AbrOptions, DefaultFetchManager, DownloadState, FetchManager, HlsConfig, HlsError,
    HlsSource, LoadedSegment, PlaylistState, SegmentRequest, SegmentState, SharedSegments,
    VariantId, VariantSizeMap, VariantState, VariantStream, build_source, make_test_source,
    set_source_coverage, set_source_variant_fence, source_can_cross_variant, source_coverage,
    source_range_ready_from_segments, source_variant_index_handle, subscribe_source_events,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    spawn_blocking,
    time::{self as time, Instant},
};
use kithara_storage::{StorageResource, WaitOutcome};
use kithara_stream::{AudioCodec, Source, StreamError, Timeline};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone, Copy)]
enum WaitRangeUnblock {
    Cancel,
    Stopped,
}

/// Create a dummy `PlaylistState` for tests (no real playlists needed).
fn dummy_playlist_state() -> Arc<PlaylistState> {
    Arc::new(PlaylistState::new(vec![]))
}

fn make_loaded_segment(
    variant: usize,
    segment_index: usize,
    byte_offset: u64,
    media_len: u64,
) -> LoadedSegment {
    LoadedSegment {
        variant,
        segment_index,
        byte_offset,
        init_len: 0,
        media_len,
        init_url: None,
        media_url: Url::parse("https://example.com/seg")
            .expect("test URL for media segment must be valid"),
    }
}

fn make_coverage_manager() -> CoverageManager<StorageResource> {
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .cancel(CancellationToken::new())
        .build();
    backend
        .open_coverage_manager()
        .expect("coverage manager should open")
}

fn set_segment_coverage(
    coverage: &CoverageManager<StorageResource>,
    url: &Url,
    total: u64,
    marks: &[Range<u64>],
) {
    let mut cov = coverage.open_state(url.to_string());
    cov.set_total_size(total);
    for range in marks {
        cov.mark(range.clone());
    }
    cov.flush();
}

fn make_variant_state_with_codec(
    id: usize,
    count: usize,
    codec: Option<AudioCodec>,
) -> VariantState {
    let base = Url::parse("https://example.com/").expect("test base URL must be valid");
    let segments = (0..count)
        .map(|index| SegmentState {
            index,
            url: base
                .join(&format!("v{id}/seg-{index}.m4s"))
                .expect("valid segment URL"),
            duration: Duration::from_secs(4),
            key: None,
        })
        .collect();

    VariantState {
        id,
        uri: base
            .join(&format!("v{id}.m3u8"))
            .expect("valid playlist URL"),
        bandwidth: Some(128_000),
        codec,
        container: None,
        init_url: None,
        segments,
        size_map: None,
    }
}

fn make_variant_state(id: usize, count: usize) -> VariantState {
    make_variant_state_with_codec(id, count, None)
}

fn uniform_size_map(segments: usize, segment_size: u64) -> VariantSizeMap {
    let offsets: Vec<u64> = (0..segments).map(|i| i as u64 * segment_size).collect();
    VariantSizeMap {
        init_size: 0,
        segment_sizes: vec![segment_size; segments],
        offsets,
        total: segments as u64 * segment_size,
    }
}

fn playlist_state_with_size_maps() -> Arc<PlaylistState> {
    let state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, 24),
        make_variant_state(1, 24),
    ]));
    state.set_size_map(0, uniform_size_map(24, 100));
    state.set_size_map(1, uniform_size_map(24, 100));
    state
}

fn playlist_state_with_codecs(
    first_codec: Option<AudioCodec>,
    second_codec: Option<AudioCodec>,
) -> Arc<PlaylistState> {
    Arc::new(PlaylistState::new(vec![
        make_variant_state_with_codec(0, 4, first_codec),
        make_variant_state_with_codec(1, 4, second_codec),
    ]))
}

fn playlist_state_without_size_map() -> Arc<PlaylistState> {
    Arc::new(PlaylistState::new(vec![make_variant_state(0, 24)]))
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

fn test_fetch_manager(cancel: CancellationToken) -> Arc<DefaultFetchManager> {
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
    let net = HttpClient::new(NetOptions::default());
    Arc::new(FetchManager::new(backend, net, cancel))
}

async fn wait_range_and_take_request(
    shared: Arc<SharedSegments>,
    mut source: HlsSource,
    range: Range<u64>,
) -> SegmentRequest {
    let handle = spawn_blocking(move || source.wait_range(range));

    let deadline = Instant::now() + Duration::from_millis(300);
    let request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        if Instant::now() > deadline {
            panic!("expected on-demand segment request");
        }
        time::sleep(Duration::from_millis(10)).await;
    };

    shared.cancel.cancel();
    shared.condvar.notify_all();
    let _ = time::timeout(Duration::from_millis(400), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic");

    request
}

#[kithara::test(tokio, browser)]
async fn seek_time_anchor_resolves_segment_and_queues_request() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let _ = shared.timeline.initiate_seek(Duration::ZERO); // epoch = 1
    // Bump to epoch 9 by calling initiate_seek 8 more times
    for _ in 0..8 {
        let _ = shared.timeline.initiate_seek(Duration::ZERO);
    }
    shared.timeline.complete_seek(9);
    shared.current_variant_index.store(0, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(anchor.variant_index, Some(0));
    assert_eq!(anchor.byte_offset, 200);
    assert_eq!(anchor.segment_start, Duration::from_secs(8));
    assert_eq!(anchor.segment_end, Some(Duration::from_secs(12)));

    let req = shared
        .segment_requests
        .pop()
        .expect("anchor seek should enqueue request");
    assert_eq!(req.variant, 0);
    assert_eq!(req.segment_index, 2);
    assert_eq!(req.seek_epoch, 9);
}

#[kithara::test]
fn media_info_uses_reader_offset_variant_instead_of_last_loaded_segment() {
    let cancel = CancellationToken::new();
    let playlist_state =
        playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::Flac));
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock();
        segments.push(make_loaded_segment(0, 0, 0, 100));
        segments.push(make_loaded_segment(1, 0, 100, 100));
    }

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    shared.timeline.set_byte_position(0);
    let info_at_start = Source::media_info(&source).expect("media info at start");
    assert_eq!(info_at_start.codec, Some(AudioCodec::AacLc));

    shared.timeline.set_byte_position(100);
    let info_after_switch = Source::media_info(&source).expect("media info at switch");
    assert_eq!(info_after_switch.codec, Some(AudioCodec::Flac));

    // Variant fence path can expose the target variant before reader_offset advances.
    shared.timeline.set_byte_position(0);
    shared.current_variant_index.store(1, Ordering::Release);
    set_source_variant_fence(&mut source, Some(0));
    let hinted_info = Source::media_info(&source).expect("media info from hinted variant");
    assert_eq!(hinted_info.codec, Some(AudioCodec::Flac));
}

#[kithara::test]
fn media_info_uses_hinted_variant_when_segments_are_flushed() {
    let cancel = CancellationToken::new();
    let playlist_state =
        playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::Flac));
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.current_variant_index.store(1, Ordering::Release);
    let info = Source::media_info(&source).expect("media info from hinted variant");
    assert_eq!(info.codec, Some(AudioCodec::Flac));
}

#[kithara::test]
fn current_segment_range_uses_reader_offset_not_last_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_codecs(None, None);
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock();
        segments.push(make_loaded_segment(0, 0, 0, 100));
        segments.push(make_loaded_segment(0, 1, 100, 100));
    }

    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.timeline.set_byte_position(0);
    assert_eq!(Source::current_segment_range(&source), Some(0..100));

    shared.timeline.set_byte_position(120);
    assert_eq!(Source::current_segment_range(&source), Some(100..200));
}

#[kithara::test]
fn format_change_segment_range_uses_metadata_when_segments_are_flushed() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.current_variant_index.store(1, Ordering::Release);
    assert_eq!(Source::format_change_segment_range(&source), Some(0..100));
}

#[kithara::test]
fn format_change_segment_range_prefers_loaded_init_bearing_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock();
        segments.push(LoadedSegment {
            variant: 1,
            segment_index: 1,
            byte_offset: 100,
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url: Url::parse("https://example.com/seg1").unwrap(),
        });
        segments.push(LoadedSegment {
            variant: 1,
            segment_index: 2,
            byte_offset: 300,
            init_len: 40,
            media_len: 100,
            init_url: Some(Url::parse("https://example.com/init").unwrap()),
            media_url: Url::parse("https://example.com/seg2").unwrap(),
        });
    }
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.current_variant_index.store(1, Ordering::Release);
    assert_eq!(Source::format_change_segment_range(&source), Some(300..440));
}

#[kithara::test]
fn format_change_segment_range_falls_back_to_metadata_without_loaded_init() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock();
        segments.push(LoadedSegment {
            variant: 1,
            segment_index: 1,
            byte_offset: 100,
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url: Url::parse("https://example.com/seg1").unwrap(),
        });
    }
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.current_variant_index.store(1, Ordering::Release);
    // Metadata for variant 1 is still the source of init-bearing segment range.
    assert_eq!(Source::format_change_segment_range(&source), Some(0..100));
}

#[kithara::test]
fn set_seek_epoch_flushes_playback_segments() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_codecs(None, None);
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock();
        segments.push(make_loaded_segment(0, 0, 0, 100));
        segments.push(make_loaded_segment(0, 1, 100, 100));
    }
    shared.timeline.set_eof(true);
    shared.timeline.set_download_position(200);
    // Set epoch to 3 via Timeline
    for _ in 0..3 {
        let _ = shared.timeline.initiate_seek(Duration::ZERO);
    }
    shared.timeline.complete_seek(3);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    Source::set_seek_epoch(&mut source, 4);

    assert_eq!(shared.timeline.seek_epoch(), 3); // epoch stays 3, set_seek_epoch no longer writes it
    assert!(!shared.timeline.eof());
    assert_eq!(shared.timeline.download_position(), 0);
    assert_eq!(shared.current_segment_index.load(Ordering::Acquire), 0);
    assert_eq!(shared.segments.lock().num_entries(), 0);
}

#[kithara::test]
fn codec_fence_allows_cross_variant_reads_when_codec_matches() {
    let cancel = CancellationToken::new();
    let playlist_state =
        playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::AacLc));
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        playlist_state,
        Timeline::new(),
    ));
    let source = make_test_source(shared, cancel);

    assert!(source_can_cross_variant(&source, 0, 1));
}

#[kithara::test]
fn codec_fence_blocks_cross_variant_reads_when_codec_changes() {
    let cancel = CancellationToken::new();
    let playlist_state =
        playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::Flac));
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        playlist_state,
        Timeline::new(),
    ));
    let source = make_test_source(shared, cancel);

    assert!(!source_can_cross_variant(&source, 0, 1));
}

#[kithara::test]
fn build_pair_seeds_timeline_total_duration_from_playlist() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, 4),
        make_variant_state(1, 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let coverage = make_coverage_manager();
    let source = build_source(
        fetch,
        &variants,
        &config,
        coverage,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    assert_eq!(
        Source::timeline(&source).total_duration(),
        Some(Duration::from_secs(16))
    );
}

#[kithara::test]
fn build_pair_seeds_current_variant_from_abr_mode() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, 4),
        make_variant_state(1, 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        abr: AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        },
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let coverage = make_coverage_manager();
    let source = build_source(
        fetch,
        &variants,
        &config,
        coverage,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    assert_eq!(
        source_variant_index_handle(&source).load(Ordering::Relaxed),
        1,
        "initial source variant should match ABR manual mode"
    );
}

#[kithara::test]
fn test_fence_at_removes_stale_entries() {
    let mut state = DownloadState::new();

    // V0 entries: 0..100, 100..200, 200..300
    state.push(make_loaded_segment(0, 0, 0, 100));
    state.push(make_loaded_segment(0, 1, 100, 100));
    state.push(make_loaded_segment(0, 2, 200, 100));

    // V3 entries: 300..400
    state.push(make_loaded_segment(3, 0, 300, 100));

    assert_eq!(state.num_entries(), 4);
    assert!(state.is_segment_loaded(0, 2));

    // Fence at offset 200, keep V3.
    // V0 entries at offset >= 200 should be removed (entry at 200..300).
    // V0 entries before offset 200 should be kept (entries at 0..100, 100..200).
    // V3 entries should be kept regardless.
    state.fence_at(200, 3);

    assert_eq!(state.num_entries(), 3);

    // V0 entries before fence are kept
    assert!(state.is_segment_loaded(0, 0));
    assert!(state.is_segment_loaded(0, 1));

    // V0 entry at/past fence is removed
    assert!(!state.is_segment_loaded(0, 2));

    // V3 entry is kept
    assert!(state.is_segment_loaded(3, 0));

    // loaded_ranges rebuilt correctly
    assert!(state.is_range_loaded(&(0..200)));
    assert!(!state.is_range_loaded(&(200..300)));
    assert!(state.is_range_loaded(&(300..400)));
}

#[kithara::test]
fn test_find_at_offset_after_fence() {
    let mut state = DownloadState::new();

    // V0: 0..100, 100..200
    state.push(make_loaded_segment(0, 0, 0, 100));
    state.push(make_loaded_segment(0, 1, 100, 100));

    // V3: 200..300
    state.push(make_loaded_segment(3, 0, 200, 100));

    // Before fence, V0 entry at 100 is findable
    assert!(state.find_at_offset(150).is_some());
    assert_eq!(state.find_at_offset(150).unwrap().variant, 0);

    // Fence at 100, keep V3
    state.fence_at(100, 3);

    // V0 entry at 100..200 removed -- offset 150 not found
    assert!(state.find_at_offset(150).is_none());

    // V0 entry at 0..100 still there
    assert!(state.find_at_offset(50).is_some());
    assert_eq!(state.find_at_offset(50).unwrap().variant, 0);

    // V3 entry at 200..300 still there
    assert!(state.find_at_offset(250).is_some());
    assert_eq!(state.find_at_offset(250).unwrap().variant, 3);
}

#[kithara::test]
fn test_wait_range_blocks_after_fence() {
    let mut state = DownloadState::new();

    // V0: 0..100, 100..200
    state.push(make_loaded_segment(0, 0, 0, 100));
    state.push(make_loaded_segment(0, 1, 100, 100));

    // V3: 200..300
    state.push(make_loaded_segment(3, 0, 200, 100));

    // Range 100..200 is loaded (V0 entry)
    assert!(state.is_range_loaded(&(100..200)));

    // Fence at 100, keep V3
    state.fence_at(100, 3);

    // Range 100..200 is no longer loaded (V0 entry removed)
    assert!(!state.is_range_loaded(&(100..200)));

    // Range 0..100 still loaded (V0 entry before fence)
    assert!(state.is_range_loaded(&(0..100)));

    // Range 200..300 still loaded (V3 entry)
    assert!(state.is_range_loaded(&(200..300)));
}

#[kithara::test]
fn test_cumulative_offset_after_switch() {
    let mut state = DownloadState::new();

    // Simulate V0 segments 0..13 occupying 0..700
    for i in 0..14 {
        state.push(make_loaded_segment(0, i, i as u64 * 50, 50));
    }

    assert_eq!(state.max_end_offset(), 700);
    assert!(state.is_range_loaded(&(0..700)));

    // Simulate variant switch: fence at 700, keep V3
    state.fence_at(700, 3);

    // V3 seg 14 placed at cumulative offset 700 (not metadata offset)
    state.push(make_loaded_segment(3, 14, 700, 200));

    // V3 seg 15 placed contiguously at 900
    state.push(make_loaded_segment(3, 15, 900, 200));

    // Verify contiguous layout with no gaps
    assert!(state.is_range_loaded(&(0..700)));
    assert!(state.is_range_loaded(&(700..900)));
    assert!(state.is_range_loaded(&(900..1100)));
    assert!(state.is_range_loaded(&(0..1100)));

    // No gap between V0 and V3
    assert!(state.find_at_offset(700).is_some());
    assert_eq!(state.find_at_offset(700).unwrap().variant, 3);
    assert_eq!(state.find_at_offset(700).unwrap().segment_index, 14);
}

#[kithara::test]
fn test_last_entry_tracks_most_recent_push() {
    let mut state = DownloadState::new();

    assert!(state.last().is_none());

    state.push(make_loaded_segment(0, 0, 0, 100));
    assert_eq!(state.last().unwrap().segment_index, 0);
    assert_eq!(state.last().unwrap().variant, 0);

    state.push(make_loaded_segment(0, 1, 100, 100));
    assert_eq!(state.last().unwrap().segment_index, 1);

    // After variant switch
    state.push(make_loaded_segment(3, 14, 200, 100));
    assert_eq!(state.last().unwrap().variant, 3);
    assert_eq!(state.last().unwrap().segment_index, 14);
}

#[kithara::test]
fn test_had_midstream_switch_flag() {
    let ps = dummy_playlist_state();
    let shared = SharedSegments::new(CancellationToken::new(), ps, Timeline::new());
    assert!(!shared.had_midstream_switch.load(Ordering::Acquire));

    shared.had_midstream_switch.store(true, Ordering::Release);
    assert!(shared.had_midstream_switch.load(Ordering::Acquire));
}

#[kithara::test]
fn test_max_end_offset() {
    let mut state = DownloadState::new();
    assert_eq!(state.max_end_offset(), 0);

    // Entries from different variants at different offsets
    state.push(make_loaded_segment(0, 0, 0, 100));
    assert_eq!(state.max_end_offset(), 100);

    state.push(make_loaded_segment(0, 1, 100, 200));
    assert_eq!(state.max_end_offset(), 300);

    // V3 entry at higher offset
    state.push(make_loaded_segment(3, 5, 500, 100));
    assert_eq!(state.max_end_offset(), 600);
}

#[kithara::test]
fn range_ready_uses_coverage_for_disk_segments() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let coverage = make_coverage_manager();
    set_source_coverage(&mut source, coverage.clone());

    let segment = make_loaded_segment(0, 0, 0, 100);
    let segment_url = segment.media_url.clone();
    {
        let mut segments = shared.segments.lock();
        segments.push(segment);
    }

    let partial_mark = 0..40;
    set_segment_coverage(&coverage, &segment_url, 100, slice::from_ref(&partial_mark));
    let segments = shared.segments.lock();
    assert!(
        !source_range_ready_from_segments(&source, &segments, &(0..80)),
        "incomplete coverage must not be treated as ready"
    );
    drop(segments);

    let full_mark = 0..100;
    set_segment_coverage(&coverage, &segment_url, 100, slice::from_ref(&full_mark));
    let segments = shared.segments.lock();
    assert!(
        source_range_ready_from_segments(&source, &segments, &(0..80)),
        "full coverage must be treated as ready"
    );
}

#[kithara::test]
fn range_ready_requires_coverage_metadata() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let source = make_test_source(Arc::clone(&shared), cancel);

    {
        let mut segments = shared.segments.lock();
        segments.push(make_loaded_segment(0, 0, 0, 100));
    }

    let segments = shared.segments.lock();
    assert!(
        !source_range_ready_from_segments(&source, &segments, &(0..80)),
        "segment without coverage metadata must not be treated as ready"
    );
}

#[kithara::test(tokio, browser)]
async fn wait_range_waits_until_coverage_is_complete() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let coverage = make_coverage_manager();
    set_source_coverage(&mut source, coverage.clone());

    let segment = make_loaded_segment(0, 0, 0, 100);
    let segment_url = segment.media_url.clone();
    {
        let mut segments = shared.segments.lock();
        segments.push(segment);
    }
    let initial_mark = 0..32;
    set_segment_coverage(&coverage, &segment_url, 100, slice::from_ref(&initial_mark));

    let shared_for_task = Arc::clone(&shared);
    let handle = spawn_blocking(move || source.wait_range(0..80));

    time::sleep(Duration::from_millis(120)).await;
    assert!(
        !handle.is_finished(),
        "wait_range returned early even though coverage is incomplete"
    );

    cancel.cancel();
    shared_for_task.condvar.notify_all();
    let result = time::timeout(Duration::from_millis(300), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic");
    assert!(result.is_err(), "wait_range should stop after cancellation");
}

// wait_range cancellation tests

#[kithara::test(tokio, browser)]
#[case(WaitRangeUnblock::Cancel)]
#[case(WaitRangeUnblock::Stopped)]
async fn test_wait_range_unblocks_with_error(#[case] unblock: WaitRangeUnblock) {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let shared2 = Arc::clone(&shared);
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    let handle = spawn_blocking(move || source.wait_range(0..1024));

    // Give wait_range time to enter the loop
    time::sleep(Duration::from_millis(20)).await;

    match unblock {
        WaitRangeUnblock::Cancel => cancel.cancel(),
        WaitRangeUnblock::Stopped => {
            shared2.stopped.store(true, Ordering::Release);
            shared2.condvar.notify_all();
        }
    }

    let result = time::timeout(Duration::from_millis(200), handle)
        .await
        .expect("task should complete within 200ms")
        .expect("task should not panic");

    assert!(
        result.is_err(),
        "wait_range should return cancellation error"
    );
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_returns_ready_when_data_pushed() {
    // Normal scenario: push segment data, wait_range returns Ready.
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let shared2 = Arc::clone(&shared);
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let coverage = source_coverage(&source);

    let handle = spawn_blocking(move || source.wait_range(0..100));

    // Push a segment covering 0..100
    time::sleep(Duration::from_millis(20)).await;
    let segment = make_loaded_segment(0, 0, 0, 100);
    let segment_url = segment.media_url.clone();
    {
        let mut segments = shared2.segments.lock();
        segments.push(segment);
    }
    let full_mark = 0..100;
    set_segment_coverage(&coverage, &segment_url, 100, slice::from_ref(&full_mark));
    shared2.condvar.notify_all();

    let result = time::timeout(Duration::from_millis(200), handle)
        .await
        .expect("task should complete within 200ms")
        .expect("task should not panic");

    assert!(matches!(result, Ok(WaitOutcome::Ready)));
}

#[kithara::test]
fn test_wait_range_flushing_interrupts_without_requesting_segment() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.current_variant_index.store(0, Ordering::Release);

    let _epoch = shared.timeline.initiate_seek(Duration::from_millis(1));

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let result = source.wait_range(150..170);
    assert!(matches!(result, Ok(WaitOutcome::Interrupted)));
    assert!(
        shared.segment_requests.pop().is_none(),
        "flushing wait_range must not enqueue on-demand requests"
    );
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_transient_eof_with_zero_total_waits_for_data() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let shared2 = Arc::clone(&shared);
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let coverage = source_coverage(&source);

    // Reproduce seek reset window: EOF flag is stale, but loaded segment state is empty.
    shared2.timeline.set_eof(true);

    let handle = spawn_blocking(move || source.wait_range(3_488_300..3_489_324));

    time::sleep(Duration::from_millis(20)).await;
    let segment = make_loaded_segment(0, 17, 3_400_000, 200_000);
    let segment_url = segment.media_url.clone();
    {
        let mut segments = shared2.segments.lock();
        segments.push(segment);
    }
    let full_mark = 0..200_000;
    set_segment_coverage(
        &coverage,
        &segment_url,
        200_000,
        slice::from_ref(&full_mark),
    );
    shared2.timeline.set_eof(false);
    shared2.condvar.notify_all();

    let result = time::timeout(Duration::from_millis(200), handle)
        .await
        .expect("task should complete within 200ms")
        .expect("task should not panic");

    assert!(matches!(result, Ok(WaitOutcome::Ready)));
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_eof_when_stopped_and_past_end() {
    // Downloader stopped + eof -- wait_range at past-end offset returns Eof.
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let shared2 = Arc::clone(&shared);

    // Push one segment
    {
        let mut segments = shared2.segments.lock();
        segments.push(make_loaded_segment(0, 0, 0, 100));
    }
    // Mark eof + stopped
    shared2.timeline.set_eof(true);
    shared2.stopped.store(true, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    let result = source.wait_range(100..200);
    assert!(matches!(result, Ok(WaitOutcome::Eof)));
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_uses_active_variant_for_seek_request() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let source = make_test_source(Arc::clone(&shared), cancel.clone());

    // Last pushed segment is from variant 1, but active playback variant is 0.
    {
        let mut segments = shared.segments.lock();
        segments.push(make_loaded_segment(1, 5, 500, 100));
    }
    shared.current_variant_index.store(0, Ordering::Release);

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 1);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_requeues_request_after_seek_epoch_change() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.current_variant_index.store(0, Ordering::Release);
    let first_epoch = shared.timeline.initiate_seek(Duration::ZERO);
    shared.timeline.complete_seek(first_epoch);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let handle = spawn_blocking(move || source.wait_range(150..170));

    let first_deadline = Instant::now() + Duration::from_millis(300);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        assert!(
            Instant::now() <= first_deadline,
            "expected initial on-demand request"
        );
        time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(first_request.seek_epoch, first_epoch);

    let second_epoch = shared.timeline.initiate_seek(Duration::from_millis(1));
    shared.timeline.complete_seek(second_epoch);
    shared.condvar.notify_all();

    let second_deadline = Instant::now() + Duration::from_millis(700);
    let second_request = loop {
        if let Some(request) = shared.segment_requests.pop()
            && request.seek_epoch == second_epoch
        {
            break request;
        }
        assert!(
            Instant::now() <= second_deadline,
            "expected re-queued on-demand request for updated seek epoch"
        );
        time::sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(second_request.variant, 0);
    assert_eq!(second_request.segment_index, 1);
    assert_eq!(second_request.seek_epoch, second_epoch);

    cancel.cancel();
    shared.condvar.notify_all();
    let result = time::timeout(Duration::from_millis(400), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic");
    assert!(result.is_err(), "wait_range should stop after cancellation");
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_without_size_map_uses_segment_zero_fallback() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_without_size_map();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.current_variant_index.store(0, Ordering::Release);
    let source = make_test_source(Arc::clone(&shared), cancel.clone());

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 0..128).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 0);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_missing_metadata_fails_fast_with_diagnostic() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let mut events = subscribe_source_events(&source);

    let handle = spawn_blocking(move || source.wait_range(1024..2048));
    let mut saw_metadata_miss = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(Ok(Event::Hls(HlsEvent::SeekMetadataMiss { .. }))) =
            time::timeout(Duration::from_millis(50), events.recv()).await
        {
            saw_metadata_miss = true;
            break;
        }
    }

    let result = time::timeout(Duration::from_secs(3), handle)
        .await
        .expect("wait_range should fail fast")
        .expect("wait_range task should not panic");

    match result {
        Err(StreamError::Source(HlsError::SegmentNotFound(message))) => {
            assert!(
                message.contains("seek metadata miss"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("unexpected wait_range result: {other:?}"),
    }

    assert!(saw_metadata_miss, "expected SeekMetadataMiss diagnostic");
}
