use std::{
    ops::{Deref, Range},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{Event, EventBus, HlsEvent};
use kithara_hls::internal::{
    AbrMode, AbrOptions, DefaultFetchManager, FetchManager, HlsConfig, HlsCoord, HlsError,
    HlsSource, PlaylistState, SegmentData, SegmentRequest, SegmentState, StreamIndex, VariantId,
    VariantSizeMap, VariantState, VariantStream, build_source, commit_dummy_resource_from_data,
    make_test_fetch_manager, make_test_source as make_internal_test_source,
    make_test_source_with_fetch as make_internal_test_source_with_fetch, set_source_variant_fence,
    source_can_cross_variant, source_variant_index_handle, subscribe_source_events,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    Mutex,
    time::{Duration, Instant, sleep, timeout},
    tokio::task::{spawn, spawn_blocking},
};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    AudioCodec, ReadOutcome, Source, SourcePhase, StreamError, Timeline, Topology,
    TransferCoordination,
};
use kithara_test_utils::{kithara, tracing_setup};
use tokio_util::sync::CancellationToken;
use url::Url;

struct SegmentRequests {
    coord: Arc<HlsCoord>,
}

impl SegmentRequests {
    fn new(coord: Arc<HlsCoord>) -> Self {
        Self { coord }
    }

    fn pop(&self) -> Option<SegmentRequest> {
        self.coord.demand().take()
    }

    fn peek(&self) -> Option<SegmentRequest> {
        self.coord.demand().peek()
    }
}

struct SharedSegments {
    coord: Arc<HlsCoord>,
    playlist_state: Arc<PlaylistState>,
    segment_requests: SegmentRequests,
    segments: Arc<Mutex<StreamIndex>>,
    timeline: Timeline,
}

impl SharedSegments {
    fn new(
        cancel: CancellationToken,
        playlist_state: Arc<PlaylistState>,
        timeline: Timeline,
    ) -> Self {
        let num_variants = playlist_state.num_variants().max(1);
        let num_segments = (0..num_variants)
            .filter_map(|v| playlist_state.num_segments(v))
            .max()
            .unwrap_or(24);
        Self::with_dimensions(cancel, playlist_state, timeline, num_variants, num_segments)
    }

    fn with_dimensions(
        cancel: CancellationToken,
        playlist_state: Arc<PlaylistState>,
        timeline: Timeline,
        num_variants: usize,
        num_segments: usize,
    ) -> Self {
        let abr_variant_index = Arc::new(AtomicUsize::new(0));
        let coord = Arc::new(HlsCoord::new(
            cancel,
            timeline.clone(),
            Arc::clone(&abr_variant_index),
        ));
        Self {
            coord: Arc::clone(&coord),
            playlist_state,
            segment_requests: SegmentRequests::new(coord),
            segments: Arc::new(Mutex::new(StreamIndex::new(num_variants, num_segments))),
            timeline,
        }
    }
}

impl Deref for SharedSegments {
    type Target = HlsCoord;

    fn deref(&self) -> &Self::Target {
        &self.coord
    }
}

fn make_test_source(shared: Arc<SharedSegments>, cancel: CancellationToken) -> HlsSource {
    make_internal_test_source(
        Arc::clone(&shared.playlist_state),
        Arc::clone(&shared.segments),
        Arc::clone(&shared.coord),
        cancel,
    )
}

fn make_test_source_with_fetch(
    shared: Arc<SharedSegments>,
    fetch: Arc<DefaultFetchManager>,
) -> HlsSource {
    make_internal_test_source_with_fetch(
        Arc::clone(&shared.playlist_state),
        Arc::clone(&shared.segments),
        Arc::clone(&shared.coord),
        fetch,
    )
}

#[derive(Clone, Copy)]
enum WaitRangeUnblock {
    Cancel,
    Stopped,
}

/// Create a dummy `PlaylistState` for tests (no real playlists needed).
fn dummy_playlist_state() -> Arc<PlaylistState> {
    Arc::new(PlaylistState::new(vec![]))
}

fn make_segment_data(media_len: u64) -> SegmentData {
    make_segment_data_with_init(0, media_len)
}

fn make_segment_data_with_init(init_len: u64, media_len: u64) -> SegmentData {
    SegmentData {
        init_len,
        media_len,
        init_url: if init_len > 0 {
            Some(
                Url::parse("https://example.com/init")
                    .expect("test URL for init segment must be valid"),
            )
        } else {
            None
        },
        media_url: Url::parse("https://example.com/seg")
            .expect("test URL for media segment must be valid"),
    }
}

fn commit_segment_bytes(
    fetch: &DefaultFetchManager,
    data: &SegmentData,
    init_bytes: &[u8],
    media_bytes: &[u8],
) {
    assert_eq!(
        data.init_len,
        init_bytes.len() as u64,
        "test init bytes must match SegmentData::init_len"
    );
    assert_eq!(
        data.media_len,
        media_bytes.len() as u64,
        "test media bytes must match SegmentData::media_len"
    );

    let backend = fetch.backend();
    let media_key = ResourceKey::from_url(&data.media_url);
    let media_res = backend
        .acquire_resource(&media_key)
        .expect("open media resource");
    media_res
        .write_at(0, media_bytes)
        .expect("write media bytes");
    media_res
        .commit(Some(media_bytes.len() as u64))
        .expect("commit media bytes");

    if let Some(init_url) = &data.init_url {
        let init_key = ResourceKey::from_url(init_url);
        let init_res = backend
            .acquire_resource(&init_key)
            .expect("open init resource");
        init_res.write_at(0, init_bytes).expect("write init bytes");
        init_res
            .commit(Some(init_bytes.len() as u64))
            .expect("commit init bytes");
    }
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
    let handle = spawn_blocking(move || source.wait_range(range, Duration::from_secs(1)));

    let deadline = Instant::now() + Duration::from_millis(300);
    let request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        if Instant::now() > deadline {
            panic!("expected on-demand segment request");
        }
        sleep(Duration::from_millis(10)).await;
    };

    shared.cancel.cancel();
    shared.condvar.notify_all();
    let _ = timeout(Duration::from_millis(400), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic");

    request
}

#[kithara::test(tokio, browser)]
async fn seek_time_anchor_resolves_segment_without_queuing_request() {
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
    shared.abr_variant_index.store(0, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(anchor.variant_index, Some(0));
    assert_eq!(anchor.byte_offset, 200);
    assert_eq!(anchor.segment_start, Duration::from_secs(8));
    assert_eq!(anchor.segment_end, Some(Duration::from_secs(12)));

    assert!(
        shared.segment_requests.pop().is_none(),
        "anchor resolution must not speculatively queue demand before decoder landing is known"
    );
}

#[kithara::test]
fn seek_time_anchor_does_not_commit_reader_position_before_decoder_lands() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.timeline.set_byte_position(100);
    shared.abr_variant_index.store(0, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    assert_eq!(anchor.segment_index, Some(2));
    assert_eq!(anchor.byte_offset, 200);
    assert_eq!(
        shared.timeline.byte_position(),
        100,
        "anchor resolution must not eagerly commit reader position before decoder landing is known"
    );
}

#[kithara::test]
fn seek_time_anchor_uses_metadata_offset_for_unloaded_tail_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(0, Ordering::Relaxed);

    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 5, make_segment_data(100));
        segments.commit_segment(0, 6, make_segment_data(100));
    }
    shared.timeline.set_byte_position(560);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(28_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    assert_eq!(anchor.segment_index, Some(7));
    assert_eq!(
        anchor.byte_offset, 700,
        "seek anchor must use metadata offset for uncommitted segment"
    );
}

#[kithara::test]
fn demand_range_uses_metadata_offset_for_unloaded_tail_offset() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(0, Ordering::Relaxed);

    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 5, make_segment_data(100));
        segments.commit_segment(0, 6, make_segment_data(100));
    }
    shared.timeline.set_byte_position(560);

    let source = make_test_source(Arc::clone(&shared), cancel);
    // Segment 6 at metadata offset 600..700, segment 7 at 700..800
    Source::demand_range(&source, 700..701);

    let req = shared
        .segment_requests
        .pop()
        .expect("demand_range must queue an on-demand segment request");
    assert_eq!(req.variant, 0);
    assert_eq!(
        req.segment_index, 7,
        "tail demand must resolve segment from metadata offset"
    );
}

#[kithara::test(tokio, browser)]
async fn wait_range_uses_metadata_offset_for_unloaded_tail_offset() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(0, Ordering::Relaxed);

    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 5, make_segment_data(100));
        segments.commit_segment(0, 6, make_segment_data(100));
    }
    shared.timeline.set_byte_position(560);

    let source = make_test_source(Arc::clone(&shared), cancel.clone());
    // Segment 7 at metadata offset 700..800
    let request = wait_range_and_take_request(Arc::clone(&shared), source, 700..701).await;
    assert_eq!(request.variant, 0);
    assert_eq!(
        request.segment_index, 7,
        "wait_range demand must resolve segment from metadata offset"
    );
}

#[kithara::test(tokio, browser)]
async fn seek_reset_wait_range_uses_same_logical_segment_as_anchor() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(1, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    let target_variant = anchor
        .variant_index
        .expect("ABR-driven seek anchor must retain target variant");
    let target_segment = anchor
        .segment_index
        .expect("seek anchor must resolve a logical segment");
    let target_segment =
        usize::try_from(target_segment).expect("test logical segment index must fit into usize");

    shared
        .segments
        .lock_sync()
        .set_layout_variant(target_variant);
    // Position the decoder at the seek anchor's byte offset in the target variant's byte space.
    shared.timeline.set_byte_position(anchor.byte_offset);

    let request = wait_range_and_take_request(
        Arc::clone(&shared),
        source,
        anchor.byte_offset..anchor.byte_offset + 1,
    )
    .await;
    assert_eq!(
        request.variant, target_variant,
        "after reset layout, wait_range must demand the same target variant chosen by seek_time_anchor"
    );
    assert_eq!(
        request.segment_index, target_segment,
        "after reset layout, wait_range must materialize the same logical segment that seek_time_anchor selected"
    );
}

#[kithara::test]
fn commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(1, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    shared.timeline.set_byte_position(150);
    Source::commit_seek_landing(&mut source, Some(anchor));

    let req = shared
        .segment_requests
        .pop()
        .expect("seek landing must enqueue request for the landed segment");
    assert_eq!(req.variant, 0, "demand uses layout_variant, not ABR target");
    assert_eq!(
        req.segment_index, 1,
        "seek landing must resolve demand from the decoder-landed byte offset"
    );
    assert_eq!(
        shared.timeline.byte_position(),
        150,
        "commit_seek_landing must not mutate the reader cursor outside Stream::seek"
    );
}

#[kithara::test]
fn commit_seek_landing_skips_request_when_landed_segment_is_ready() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(1, Ordering::Relaxed);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let seg0_data = make_segment_data(100);
    let seg1_data = make_segment_data(100);
    commit_dummy_resource_from_data(&source, &seg1_data);
    {
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        // Commit v1 seg 0 so that seg 1 starts at byte offset 100.
        segments.commit_segment(1, 0, seg0_data);
        segments.commit_segment(1, 1, seg1_data);
    }
    shared.timeline.set_byte_position(150);

    Source::commit_seek_landing(&mut source, None);

    assert!(
        shared.segment_requests.pop().is_none(),
        "ready landed segment must not enqueue redundant downloader work"
    );
    assert_eq!(
        Source::current_segment_range(&source),
        Some(100..200),
        "landed ready segment must be derived from canonical layout and reader offset"
    );
}

#[kithara::test]
fn commit_seek_landing_fences_stale_variants_at_landed_offset() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    shared.abr_variant_index.store(1, Ordering::Relaxed);
    shared.timeline.set_byte_position(550);
    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 0, make_segment_data(100));
        segments.commit_segment(0, 1, make_segment_data(100));
        segments.commit_segment(1, 5, make_segment_data(100));
    }

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(500))
        .expect("seek anchor resolution should succeed")
        .expect("HLS source should resolve anchor");

    // Seek uses layout_variant (0), not ABR target (1).
    assert_eq!(anchor.variant_index, Some(0));
    assert_eq!(anchor.segment_index, Some(0));
    assert_eq!(anchor.byte_offset, 0);

    shared.timeline.set_byte_position(0);
    Source::commit_seek_landing(&mut source, Some(anchor));

    let segments = shared.segments.lock_sync();
    // Seek does NOT switch layout_variant. ABR switch happens via format_change.
    assert_eq!(
        segments.layout_variant(),
        0,
        "seek must not switch layout_variant — ABR takes effect via format_change"
    );
    assert!(
        segments.is_segment_loaded(0, 0),
        "layout-variant entries must remain available after seek"
    );
    drop(segments);

    // Recovery requests target layout_variant (0), not ABR target.
    if let Some(request) = shared.segment_requests.pop() {
        assert_eq!(request.variant, 0);
    }
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
        let mut segments = shared.segments.lock_sync();
        // Commit v0 seg 0 in variant 0's byte map (layout_variant defaults to 0).
        segments.commit_segment(0, 0, make_segment_data(100));
        // Commit v1 seg 0 and seg 1 in variant 1's byte map.
        segments.commit_segment(1, 0, make_segment_data(100));
        segments.commit_segment(1, 1, make_segment_data(100));
    }

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    // layout_variant=0, byte_position 0 → v0 seg 0 → AacLc
    shared.timeline.set_byte_position(0);
    let info_at_start = Source::media_info(&source).expect("media info at start");
    assert_eq!(info_at_start.codec, Some(AudioCodec::AacLc));

    // Switch layout to variant 1; byte_position 0 → v1 seg 0 → Flac
    shared.segments.lock_sync().set_layout_variant(1);
    shared.timeline.set_byte_position(0);
    let info_after_switch = Source::media_info(&source).expect("media info at switch");
    assert_eq!(info_after_switch.codec, Some(AudioCodec::Flac));

    // Variant fence path can expose the target variant before reader_offset advances.
    shared.segments.lock_sync().set_layout_variant(0);
    shared.timeline.set_byte_position(0);
    shared.abr_variant_index.store(1, Ordering::Release);
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

    shared.abr_variant_index.store(1, Ordering::Release);
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
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 0, make_segment_data(100));
        segments.commit_segment(0, 1, make_segment_data(100));
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

    shared.abr_variant_index.store(1, Ordering::Release);
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
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(
            1,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg1").unwrap(),
            },
        );
        segments.commit_segment(
            1,
            2,
            SegmentData {
                init_len: 40,
                media_len: 100,
                init_url: Some(Url::parse("https://example.com/init").unwrap()),
                media_url: Url::parse("https://example.com/seg2").unwrap(),
            },
        );
    }
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.abr_variant_index.store(1, Ordering::Release);
    // In variant 1's byte map: seg1 at 0..100, seg2 at 100..240 (init 40 + media 100)
    assert_eq!(Source::format_change_segment_range(&source), Some(100..240));
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
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(
            1,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg1").unwrap(),
            },
        );
    }
    let source = make_test_source(Arc::clone(&shared), cancel);

    shared.abr_variant_index.store(1, Ordering::Release);
    // Metadata for variant 1 is still the source of init-bearing segment range.
    assert_eq!(Source::format_change_segment_range(&source), Some(0..100));
}

#[kithara::test]
fn format_change_segment_range_reads_self_contained_bytes_from_reset_layout_floor() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let fetch = make_test_fetch_manager(cancel.clone());
    let write_fetch = Arc::clone(&fetch);
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);
    let init_bytes = b"v1-init";
    let media_bytes = b"v1-seg2";
    let mut expected = Vec::new();
    expected.extend_from_slice(init_bytes);
    expected.extend_from_slice(media_bytes);
    let segment_data =
        make_segment_data_with_init(init_bytes.len() as u64, media_bytes.len() as u64);

    commit_segment_bytes(write_fetch.as_ref(), &segment_data, init_bytes, media_bytes);
    {
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(1, 2, segment_data);
    }
    shared.abr_variant_index.store(1, Ordering::Release);

    // In variant 1's per-variant byte map, seg 2 is the first committed segment.
    // With expected_sizes not set for variant 1, seg 2 starts at offset 0 in v1's byte space
    // (per-variant byte maps don't share offsets with other variants).
    // TODO: verify this test reflects per-variant byte map semantics
    let seg2_offset = {
        let segments = shared.segments.lock_sync();
        segments
            .find_at_offset(0)
            .map(|s| s.byte_offset)
            .unwrap_or(0)
    };
    assert_eq!(
        Source::format_change_segment_range(&source),
        Some(seg2_offset..seg2_offset + expected.len() as u64),
        "decoder-start boundary must use the segment's position in the per-variant byte map"
    );

    let mut buf = vec![0u8; expected.len()];
    let read = source
        .read_at(seg2_offset, &mut buf)
        .expect("read_at from decoder-start boundary");

    assert_eq!(
        read,
        ReadOutcome::Data(expected.len()),
        "decoder-start boundary must be immediately readable"
    );
    assert_eq!(
        buf, expected,
        "reading from the reported decoder-start boundary must return init bytes followed by media bytes"
    );
}

#[kithara::test]
fn reset_layout_reads_late_loaded_segment_at_absolute_offset() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let fetch = make_test_fetch_manager(cancel.clone());
    let write_fetch = Arc::clone(&fetch);
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);
    let segment_data = make_segment_data(100);
    let expected: Vec<u8> = (0u8..100).collect();

    commit_segment_bytes(write_fetch.as_ref(), &segment_data, &[], &expected);
    {
        let mut segments = shared.segments.lock_sync();
        segments.set_expected_sizes(0, vec![100; 24]);
        segments.set_expected_sizes(1, vec![100; 24]);
        segments.set_layout_variant(1);
        segments.commit_segment(1, 7, segment_data);
    }
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.timeline.set_byte_position(750);

    assert_eq!(
        Source::current_segment_range(&source),
        Some(700..800),
        "after SeekLayout::Reset, late loaded segments must keep absolute layout offsets instead of collapsing to the reset floor"
    );
    assert_eq!(
        Source::phase(&source),
        SourcePhase::Ready,
        "source readiness at the landed byte position must use the absolute layout offset of the loaded segment"
    );

    // Read from offset 750 (50 bytes into the segment starting at 700).
    let read_len = 50;
    let mut buf = vec![0u8; read_len];
    let read = source
        .read_at(750, &mut buf)
        .expect("read_at from absolute late-segment offset");

    assert_eq!(
        read,
        ReadOutcome::Data(read_len),
        "read_at must return committed bytes for a late loaded segment at its absolute layout offset"
    );
    assert_eq!(
        buf,
        &expected[50..],
        "read_at must resolve the committed late segment using absolute layout coordinates"
    );
}

#[kithara::test]
fn mixed_layout_read_at_returns_expected_bytes_across_variant_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    let fetch = make_test_fetch_manager(cancel.clone());
    let write_fetch = Arc::clone(&fetch);
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);

    let seg0_bytes = b"v0-seg0|";
    let seg1_bytes = b"v0-seg1|";
    let init2_bytes = b"v1-init|";
    let seg2_bytes = b"v1-seg2|";

    let seg0 = SegmentData {
        init_len: 0,
        media_len: seg0_bytes.len() as u64,
        init_url: None,
        media_url: Url::parse("https://example.com/v0-seg0").expect("valid URL"),
    };
    let seg1 = SegmentData {
        init_len: 0,
        media_len: seg1_bytes.len() as u64,
        init_url: None,
        media_url: Url::parse("https://example.com/v0-seg1").expect("valid URL"),
    };
    let seg2 = SegmentData {
        init_len: init2_bytes.len() as u64,
        media_len: seg2_bytes.len() as u64,
        init_url: Some(Url::parse("https://example.com/v1-init2").expect("valid URL")),
        media_url: Url::parse("https://example.com/v1-seg2").expect("valid URL"),
    };

    commit_segment_bytes(write_fetch.as_ref(), &seg0, &[], seg0_bytes);
    commit_segment_bytes(write_fetch.as_ref(), &seg1, &[], seg1_bytes);
    commit_segment_bytes(write_fetch.as_ref(), &seg2, init2_bytes, seg2_bytes);

    // TODO: verify this test reflects per-variant byte map semantics
    // With per-variant byte maps, each variant has its own contiguous byte space.
    // We commit all segments to variant 1 to test contiguous reads within a single variant.
    {
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(1, 0, seg0);
        segments.commit_segment(1, 1, seg1);
        segments.commit_segment(1, 2, seg2);
    }
    shared.abr_variant_index.store(1, Ordering::Release);

    let mut expected = Vec::new();
    expected.extend_from_slice(seg0_bytes);
    expected.extend_from_slice(seg1_bytes);
    expected.extend_from_slice(init2_bytes);
    expected.extend_from_slice(seg2_bytes);

    let mut actual = Vec::new();
    let mut offset = 0u64;
    while actual.len() < expected.len() {
        let mut buf = [0u8; 5];
        let read = source
            .read_at(offset, &mut buf)
            .expect("read_at over mixed layout");
        match read {
            ReadOutcome::Data(0) => {
                panic!("unexpected EOF while reading mixed-layout byte stream at offset {offset}")
            }
            ReadOutcome::Data(n) => {
                actual.extend_from_slice(&buf[..n]);
                offset += n as u64;
            }
            other => panic!("expected committed bytes from mixed layout, got {other:?}"),
        }
    }

    assert_eq!(
        actual, expected,
        "mixed layout must expose the exact expected byte stream across the variant switch"
    );
}

#[kithara::test]
fn set_seek_epoch_is_non_destructive() {
    let cancel = CancellationToken::new();
    let playlist_state = playlist_state_with_codecs(None, None);
    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&playlist_state),
        Timeline::new(),
    ));
    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(0, 0, make_segment_data(100));
        segments.commit_segment(0, 1, make_segment_data(100));
    }
    shared.timeline.set_eof(true);
    shared.timeline.set_download_position(200);
    shared.had_midstream_switch.store(true, Ordering::Release);
    // Set epoch to 3 via Timeline
    for _ in 0..3 {
        let _ = shared.timeline.initiate_seek(Duration::ZERO);
    }
    shared.timeline.complete_seek(3);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    Source::set_seek_epoch(&mut source, 4);

    // Non-destructive: segments, download_position, and exact-EOF visibility
    // are preserved. Only the midstream-switch coordination flag is reset.
    assert_eq!(shared.timeline.seek_epoch(), 3);
    assert!(shared.timeline.eof());
    assert_eq!(shared.timeline.download_position(), 200);
    assert!(!shared.had_midstream_switch.load(Ordering::Acquire));
    assert_eq!(shared.segments.lock_sync().num_committed(), 2);
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
    let source = build_source(
        fetch,
        &variants,
        &config,
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
    let source = build_source(
        fetch,
        &variants,
        &config,
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
fn test_switch_variant_remaps_layout() {
    let mut state = StreamIndex::new(4, 24);

    // V0 entries: segments 0, 1, 2 (each 100 bytes)
    state.commit_segment(0, 0, make_segment_data(100));
    state.commit_segment(0, 1, make_segment_data(100));
    state.commit_segment(0, 2, make_segment_data(100));

    assert_eq!(state.num_committed(), 3);

    // Switch active layout to variant 3, commit v3 seg 2.
    // Per-variant byte maps: v0 and v3 have independent byte spaces.
    state.set_layout_variant(3);
    state.commit_segment(3, 2, make_segment_data(100));

    // num_committed() counts entries in layout_variant's byte_map.
    // layout_variant is 3, which has 1 committed segment.
    assert_eq!(state.num_committed(), 1);

    // V0 entries are still stored
    assert!(state.is_segment_loaded(0, 0));
    assert!(state.is_segment_loaded(0, 1));

    // V0 entries are in v0's byte layout (switch back to check)
    state.set_layout_variant(0);
    assert!(state.is_range_loaded(&(0..200)));

    // V3's byte map has seg 2 at offset 0 (only committed segment in v3)
    state.set_layout_variant(3);
    assert!(state.find_at_offset(0).is_some());
    assert_eq!(state.find_at_offset(0).unwrap().variant, 3);
    assert_eq!(state.find_at_offset(0).unwrap().segment_index, 2);
}

#[kithara::test]
fn test_find_at_offset_after_switch_variant() {
    let mut state = StreamIndex::new(4, 24);

    // V0: segments 0, 1 (each 100 bytes → 0..100, 100..200 in v0's byte map)
    state.commit_segment(0, 0, make_segment_data(100));
    state.commit_segment(0, 1, make_segment_data(100));

    // With layout_variant=0, V0 entry at 100..200 is findable
    assert!(state.find_at_offset(150).is_some());
    assert_eq!(state.find_at_offset(150).unwrap().variant, 0);

    // Commit v3 seg 1 and seg 2 (each 100 bytes → 0..100, 100..200 in v3's byte map)
    state.commit_segment(3, 1, make_segment_data(100));
    state.commit_segment(3, 2, make_segment_data(100));

    // Switch to variant 3's byte map
    state.set_layout_variant(3);

    // v3 seg 1 at 0..100 (no expected_sizes, seg 0 absent → seg 1 starts at 0)
    let seg = state.find_at_offset(0).unwrap();
    assert_eq!(seg.variant, 3);
    assert_eq!(seg.segment_index, 1);

    let seg = state.find_at_offset(100).unwrap();
    assert_eq!(seg.variant, 3);
    assert_eq!(seg.segment_index, 2);

    // Switch back to V0: V0 entries still findable
    state.set_layout_variant(0);
    assert!(state.find_at_offset(50).is_some());
    assert_eq!(state.find_at_offset(50).unwrap().variant, 0);
}

#[kithara::test]
fn test_range_loaded_after_switch_variant() {
    let mut state = StreamIndex::new(4, 24);

    // V0: segments 0, 1 (each 100 bytes → 0..100, 100..200 in v0's byte map)
    state.commit_segment(0, 0, make_segment_data(100));
    state.commit_segment(0, 1, make_segment_data(100));

    // Range 100..200 is loaded in v0's byte map (layout_variant=0)
    assert!(state.is_range_loaded(&(100..200)));

    // Commit v3 segments 1 and 2 (each 100 bytes → v3 byte map: 0..100, 100..200)
    state.commit_segment(3, 1, make_segment_data(100));
    state.commit_segment(3, 2, make_segment_data(100));

    // V0 byte map: seg 0 at 0..100, seg 1 at 100..200
    assert!(state.is_range_loaded(&(0..100)));
    assert!(state.is_range_loaded(&(0..200)));

    // Switch to v3 byte map
    state.set_layout_variant(3);

    // v3 byte map: seg 1 at 0..100, seg 2 at 100..200
    assert!(state.is_range_loaded(&(0..100)));
    assert!(state.is_range_loaded(&(100..200)));
    assert!(state.is_range_loaded(&(0..200)));
}

#[kithara::test]
fn test_cumulative_offset_after_switch() {
    let mut state = StreamIndex::new(4, 24);

    // Simulate V0 segments 0..13 occupying 0..700 in v0's byte map
    for i in 0..14 {
        state.commit_segment(0, i, make_segment_data(50));
    }

    assert_eq!(state.max_end_offset(), 700);
    assert!(state.is_range_loaded(&(0..700)));

    // Switch to variant 3's byte map
    state.set_layout_variant(3);

    // V3 seg 14 and 15 (no expected_sizes, segs 0-13 absent → contiguous from 0)
    state.commit_segment(3, 14, make_segment_data(200));
    state.commit_segment(3, 15, make_segment_data(200));

    // V3 byte map: seg 14 at 0..200, seg 15 at 200..400
    assert!(state.is_range_loaded(&(0..200)));
    assert!(state.is_range_loaded(&(200..400)));
    assert!(state.is_range_loaded(&(0..400)));

    assert!(state.find_at_offset(0).is_some());
    assert_eq!(state.find_at_offset(0).unwrap().variant, 3);
    assert_eq!(state.find_at_offset(0).unwrap().segment_index, 14);

    // V0 byte map still intact
    state.set_layout_variant(0);
    assert_eq!(state.max_end_offset(), 700);
    assert!(state.is_range_loaded(&(0..700)));
}

#[kithara::test]
fn test_max_end_offset_tracks_committed_watermark() {
    let mut state = StreamIndex::new(4, 24);

    assert_eq!(state.max_end_offset(), 0);

    state.commit_segment(0, 0, make_segment_data(100));
    assert_eq!(state.max_end_offset(), 100);

    // find_at_offset at highest byte still works
    let seg = state.find_at_offset(99).unwrap();
    assert_eq!(seg.variant, 0);
    assert_eq!(seg.segment_index, 0);

    state.commit_segment(0, 1, make_segment_data(100));
    assert_eq!(state.max_end_offset(), 200);

    // After switching layout variant, max_end_offset reflects the new variant's byte map
    state.set_layout_variant(3);
    state.commit_segment(3, 2, make_segment_data(100));
    // V3 byte map: seg 2 at 0..100 (no expected_sizes, segs 0-1 absent)
    assert_eq!(state.max_end_offset(), 100);

    // V0 byte map watermark is preserved
    state.set_layout_variant(0);
    assert_eq!(state.max_end_offset(), 200);
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
    let mut state = StreamIndex::new(4, 24);
    assert_eq!(state.max_end_offset(), 0);

    state.commit_segment(0, 0, make_segment_data(100));
    assert_eq!(state.max_end_offset(), 100);

    state.commit_segment(0, 1, make_segment_data(200));
    assert_eq!(state.max_end_offset(), 300);

    // V3 entry after switching layout variant
    state.set_layout_variant(3);
    state.commit_segment(3, 2, make_segment_data(100));
    // V3 byte map: seg 2 at 0..100 (no segs 0-1, no expected_sizes)
    assert_eq!(state.max_end_offset(), 100);
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

    let handle = spawn_blocking(move || source.wait_range(0..1024, Duration::from_secs(1)));

    // Give wait_range time to enter the loop
    sleep(Duration::from_millis(20)).await;

    match unblock {
        WaitRangeUnblock::Cancel => cancel.cancel(),
        WaitRangeUnblock::Stopped => {
            shared2.stopped.store(true, Ordering::Release);
            shared2.condvar.notify_all();
        }
    }

    let result = timeout(Duration::from_millis(200), handle)
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
    let fetch = make_test_fetch_manager(cancel.clone());
    // commit_source shares the same fetch backend for writing dummy resources.
    let commit_source = make_test_source_with_fetch(Arc::clone(&shared), Arc::clone(&fetch));
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);

    let handle = spawn_blocking(move || source.wait_range(0..100, Duration::from_secs(1)));

    // Push a segment covering 0..100 and commit its resource data
    sleep(Duration::from_millis(20)).await;
    let segment_data = make_segment_data(100);
    commit_dummy_resource_from_data(&commit_source, &segment_data);
    {
        let mut segments = shared2.segments.lock_sync();
        segments.commit_segment(0, 0, segment_data);
    }
    shared2.condvar.notify_all();

    let result = timeout(Duration::from_millis(200), handle)
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
    shared.abr_variant_index.store(0, Ordering::Release);

    let _epoch = shared.timeline.initiate_seek(Duration::from_millis(1));

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let result = source.wait_range(150..170, Duration::from_secs(1));
    assert!(matches!(result, Ok(WaitOutcome::Interrupted)));
    assert!(
        shared.segment_requests.pop().is_none(),
        "flushing wait_range must not enqueue on-demand requests"
    );
}

#[kithara::test]
fn test_wait_range_flushing_overrides_stale_eof() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let total_bytes = 2_400u64;
    shared.timeline.set_eof(true);
    shared.timeline.set_byte_position(total_bytes);

    let _ = shared.timeline.initiate_seek(Duration::ZERO);
    let mut source = make_test_source(Arc::clone(&shared), cancel);

    let result = source.wait_range(total_bytes..total_bytes + 128, Duration::from_secs(1));

    assert!(
        shared.timeline.is_flushing(),
        "test setup must remain in flushing state before source apply"
    );
    assert!(matches!(result, Ok(WaitOutcome::Interrupted)));
}

#[kithara::test]
fn test_wait_range_stale_eof_overrides_multiple_initiate_seek_storm() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);

    let total_bytes = 2_400u64;
    shared.timeline.set_eof(true);
    shared.timeline.set_byte_position(total_bytes);

    // Simulate holding the seek key: multiple seek starts without completing the newest one yet.
    for idx in 0..12u64 {
        let _ = shared
            .timeline
            .initiate_seek(Duration::from_millis(idx + 1));
    }

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let result = source.wait_range(total_bytes..total_bytes + 128, Duration::from_secs(1));

    assert!(
        shared.timeline.is_flushing(),
        "seek storm must keep timeline in flushing state"
    );
    assert!(
        matches!(result, Ok(WaitOutcome::Interrupted)),
        "stale EOF should not end playback while seek flushing is active"
    );
    assert!(shared.timeline.eof(), "bug reproducer relies on stale EOF");
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_transient_eof_with_zero_total_waits_for_data() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let shared2 = Arc::clone(&shared);
    let fetch = make_test_fetch_manager(cancel.clone());
    let commit_source = make_test_source_with_fetch(Arc::clone(&shared), Arc::clone(&fetch));
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);

    // Reproduce seek reset window: EOF flag is stale, but loaded segment state is empty.
    shared2.timeline.set_eof(true);

    let handle = spawn_blocking(move || source.wait_range(88_300..89_324, Duration::from_secs(1)));

    sleep(Duration::from_millis(20)).await;
    let segment_data = make_segment_data(200_000);
    commit_dummy_resource_from_data(&commit_source, &segment_data);
    {
        let mut segments = shared2.segments.lock_sync();
        segments.commit_segment(0, 0, segment_data);
    }
    shared2.timeline.set_eof(false);
    shared2.condvar.notify_all();

    let result = timeout(Duration::from_millis(200), handle)
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
        let mut segments = shared2.segments.lock_sync();
        segments.commit_segment(0, 0, make_segment_data(100));
    }
    // Mark eof + stopped
    shared2.timeline.set_eof(true);
    shared2.stopped.store(true, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    let result = source.wait_range(100..200, Duration::from_secs(1));
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
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(1, 5, make_segment_data(100));
    }
    shared.abr_variant_index.store(0, Ordering::Release);

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 1);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_uses_variant_fence_when_abr_hint_changes() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    // Playback is fenced to variant 0, but ABR hint moved to variant 1.
    set_source_variant_fence(&mut source, Some(0));
    shared.abr_variant_index.store(1, Ordering::Release);

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 1);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_uses_layout_variant_after_midstream_switch() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    // The read path is still fenced to variant 0, but downloader has already
    // committed to a midstream switch and drained on-demand requests.
    set_source_variant_fence(&mut source, Some(0));
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
    assert_eq!(
        request.variant, 0,
        "demand uses layout_variant, not ABR target"
    );
    assert_eq!(request.segment_index, 1);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_clamps_to_target_floor_after_midstream_switch() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    set_source_variant_fence(&mut source, Some(0));
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);

    {
        let mut segments = shared.segments.lock_sync();
        segments.commit_segment(1, 6, make_segment_data(100));
    }

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
    assert_eq!(
        request.variant, 0,
        "demand uses layout_variant, not ABR target"
    );
    assert_eq!(
        request.segment_index, 1,
        "with layout_variant=0 and 100-byte segments, offset 150 maps to segment 1"
    );
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_uses_layout_owned_segment_in_switched_tail() {
    let cancel = CancellationToken::new();
    let ps = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, 24),
        make_variant_state(1, 24),
    ]));
    ps.set_size_map(0, uniform_size_map(24, 50));
    ps.set_size_map(1, uniform_size_map(24, 700));

    let shared = Arc::new(SharedSegments::new(
        cancel.clone(),
        Arc::clone(&ps),
        Timeline::new(),
    ));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    set_source_variant_fence(&mut source, Some(0));
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);

    {
        let mut segments = shared.segments.lock_sync();
        for segment_index in 0..4 {
            segments.commit_segment(0, segment_index, make_segment_data(50));
        }
        // Set expected sizes for variant 1 so byte offsets are correct.
        if let Some(sizes) = ps.segment_sizes(1) {
            segments.set_expected_sizes(1, sizes);
        }
        segments.set_layout_variant(1);
        segments.commit_segment(1, 4, make_segment_data(700));
    }

    // With per-variant byte maps and layout_variant=1, v1 expected sizes = 700 bytes/seg.
    // Offset 910 maps to seg 1 (910 / 700 = 1). Demand uses layout_variant directly.
    let request = wait_range_and_take_request(Arc::clone(&shared), source, 910..930).await;
    assert_eq!(
        request.variant, 1,
        "demand uses layout_variant (which is 1 after set_layout_variant)"
    );
    assert_eq!(
        request.segment_index, 1,
        "with layout_variant=1 and 700-byte segments, offset 910 maps to segment 1"
    );
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_requeues_request_after_seek_epoch_change() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    let first_epoch = shared.timeline.initiate_seek(Duration::ZERO);
    shared.timeline.complete_seek(first_epoch);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(1)));

    let first_deadline = Instant::now() + Duration::from_millis(300);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        assert!(
            Instant::now() <= first_deadline,
            "expected initial on-demand request"
        );
        sleep(Duration::from_millis(10)).await;
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
        sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(second_request.variant, 0);
    assert_eq!(second_request.segment_index, 1);
    assert_eq!(second_request.seek_epoch, second_epoch);

    cancel.cancel();
    shared.condvar.notify_all();
    let result = timeout(Duration::from_millis(400), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic");
    assert!(result.is_err(), "wait_range should stop after cancellation");
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_interrupts_stale_range_after_applied_seek_epoch_change() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    let first_epoch = shared.timeline.initiate_seek(Duration::ZERO);
    shared.timeline.complete_seek(first_epoch);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(1)));

    let first_deadline = Instant::now() + Duration::from_millis(300);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        assert!(
            Instant::now() <= first_deadline,
            "expected initial on-demand request"
        );
        sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(first_request.seek_epoch, first_epoch);

    let second_epoch = shared.timeline.initiate_seek(Duration::from_millis(1));
    shared.timeline.set_byte_position(50);
    shared.timeline.complete_seek(second_epoch);
    shared.timeline.clear_seek_pending(second_epoch);
    shared.condvar.notify_all();

    let result = timeout(Duration::from_millis(400), handle)
        .await
        .expect("wait_range task should complete")
        .expect("wait_range task should not panic")
        .expect("wait_range should exit after applied seek");
    assert!(
        matches!(result, WaitOutcome::Interrupted),
        "old wait_range must interrupt after a newer seek has already been applied"
    );
    assert!(
        shared
            .segment_requests
            .pop()
            .is_none_or(|request| request.seek_epoch != second_epoch),
        "stale range must not enqueue a new on-demand request for the newer seek epoch"
    );
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_without_size_map_uses_segment_zero_fallback() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_without_size_map();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    let source = make_test_source(Arc::clone(&shared), cancel.clone());

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 0..128).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 0);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_without_size_map_emits_seek_fallback_diagnostic() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_without_size_map();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    let source = make_test_source(Arc::clone(&shared), cancel.clone());
    let mut events = subscribe_source_events(&source);

    let request = wait_range_and_take_request(Arc::clone(&shared), source, 0..128).await;
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 0);

    let diag = timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(Event::Hls(HlsEvent::Seek {
                    stage,
                    seek_epoch,
                    variant,
                    offset,
                    from_segment_index,
                    to_segment_index,
                })) if stage == "wait_range_metadata_fallback" => {
                    break (
                        stage,
                        seek_epoch,
                        variant,
                        offset,
                        from_segment_index,
                        to_segment_index,
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    panic!("expected seek fallback diagnostic, got channel error: {error}")
                }
            }
        }
    })
    .await
    .expect("seek fallback diagnostic should arrive");

    assert_eq!(diag.0, "wait_range_metadata_fallback");
    assert_eq!(diag.1, 0);
    assert_eq!(diag.2, 0);
    assert_eq!(diag.3, 0);
    assert_eq!(diag.4, 0);
    assert_eq!(diag.5, 0);
}

#[kithara::test(tokio, browser)]
async fn test_wait_range_missing_metadata_fails_fast_with_diagnostic() {
    let cancel = CancellationToken::new();
    let ps = dummy_playlist_state();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let mut events = subscribe_source_events(&source);

    let handle = spawn_blocking(move || source.wait_range(1024..2048, Duration::from_secs(1)));
    let mut saw_metadata_miss = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(Ok(Event::Hls(HlsEvent::SeekMetadataMiss { .. }))) =
            timeout(Duration::from_millis(50), events.recv()).await
        {
            saw_metadata_miss = true;
            break;
        }
    }

    let result = timeout(Duration::from_secs(3), handle)
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

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_wait_range_stalled_on_demand_request_is_interrupted_by_flush(_tracing_setup: ()) {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel);
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(1)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected on-demand request before wasm stall timeout"
        );
    };

    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 1);

    let _ = shared.timeline.initiate_seek(Duration::from_millis(1));
    shared.condvar.notify_all();

    let result = timeout(Duration::from_secs(2), handle)
        .await
        .expect("wait_range should exit after seek flush")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Interrupted)));
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_wait_range_stalled_on_demand_request_becomes_ready_when_segment_arrives() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    let fetch = make_test_fetch_manager(cancel.clone());
    let commit_source = make_test_source_with_fetch(Arc::clone(&shared), Arc::clone(&fetch));
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);

    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(1)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected on-demand request before timeout"
        );
    };
    assert_eq!(request.variant, 0);
    assert_eq!(request.segment_index, 1);

    {
        let seg0_data = make_segment_data(100);
        let seg1_data = make_segment_data(100);
        commit_dummy_resource_from_data(&commit_source, &seg1_data);
        let mut segments = shared.segments.lock_sync();
        // Commit seg 0 first so seg 1 starts at byte offset 100 (covering 150..170).
        segments.commit_segment(0, 0, seg0_data);
        segments.commit_segment(0, 1, seg1_data);
    }
    shared.condvar.notify_all();

    let result = timeout(Duration::from_secs(2), handle)
        .await
        .expect("wait_range should not hang while data is pushed")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Ready)));
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(8)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn test_wait_range_stalled_on_demand_request_is_not_duplicated() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(30)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.peek() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected an on-demand request before timeout"
        );
    };
    assert_eq!(first_request.variant, 0);
    assert_eq!(first_request.segment_index, 1);

    let dedupe_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() <= dedupe_deadline {
        assert!(
            shared.segment_requests.peek() == Some(first_request),
            "wait_range should keep a single stable on-demand request while stalled"
        );
        shared.condvar.notify_all();
        sleep(Duration::from_millis(10)).await;
    }

    cancel.cancel();
    shared.condvar.notify_all();
    let result = timeout(Duration::from_millis(500), handle)
        .await
        .expect("wait_range should exit after cancellation")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Interrupted) | Err(_)));
}

#[kithara::test(tokio, browser, env(KITHARA_HANG_TIMEOUT_SECS = "3"))]
async fn test_wait_range_midstream_switch_repush_is_not_duplicated() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(30)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.peek() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected an on-demand request before timeout"
        );
    };
    assert_eq!(first_request.variant, 0);
    assert_eq!(first_request.segment_index, 1);

    let dedupe_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() <= dedupe_deadline {
        assert!(
            shared.segment_requests.peek() == Some(first_request),
            "midstream-switch wake storms must not replace the stable demand slot"
        );
        shared.condvar.notify_all();
        sleep(Duration::from_millis(10)).await;
    }

    cancel.cancel();
    shared.condvar.notify_all();
    let result = timeout(Duration::from_millis(500), handle)
        .await
        .expect("wait_range should exit after cancellation")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Interrupted) | Err(_)));
}

#[kithara::test(tokio, browser, env(KITHARA_HANG_TIMEOUT_SECS = "3"))]
async fn test_wait_range_midstream_switch_target_request_stays_stable_until_ready() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
    set_source_variant_fence(&mut source, Some(0));
    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(30)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let first_request = loop {
        if let Some(request) = shared.segment_requests.peek() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected an on-demand request before timeout"
        );
    };
    assert_eq!(
        first_request.variant, 0,
        "demand uses layout_variant, not ABR target"
    );
    assert_eq!(first_request.segment_index, 1);

    let dedupe_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() <= dedupe_deadline {
        assert_eq!(
            shared.segment_requests.peek(),
            Some(first_request),
            "post-switch repush must keep the target-variant demand stable until the range becomes ready"
        );
        shared.condvar.notify_all();
        sleep(Duration::from_millis(10)).await;
    }

    cancel.cancel();
    shared.condvar.notify_all();
    let result = timeout(Duration::from_millis(500), handle)
        .await
        .expect("wait_range should exit after cancellation")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Interrupted) | Err(_)));
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_wait_range_clears_midstream_switch_after_target_range_becomes_ready() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps();
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(1, Ordering::Release);
    shared.had_midstream_switch.store(true, Ordering::Release);
    let fetch = test_fetch_manager(cancel.clone());
    let commit_source = make_test_source_with_fetch(Arc::clone(&shared), Arc::clone(&fetch));
    let mut source = make_test_source_with_fetch(Arc::clone(&shared), fetch);
    set_source_variant_fence(&mut source, Some(0));

    let handle = spawn_blocking(move || source.wait_range(150..170, Duration::from_secs(1)));

    let request_deadline = Instant::now() + Duration::from_secs(2);
    let request = loop {
        if let Some(request) = shared.segment_requests.pop() {
            break request;
        }
        sleep(Duration::from_millis(10)).await;
        assert!(
            Instant::now() <= request_deadline,
            "expected on-demand request before timeout"
        );
    };
    assert_eq!(
        request.variant, 0,
        "demand uses layout_variant, not ABR target"
    );
    assert_eq!(request.segment_index, 1);

    {
        let seg0_data = make_segment_data(100);
        let seg1_data = make_segment_data(100);
        commit_dummy_resource_from_data(&commit_source, &seg1_data);
        let mut segments = shared.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(1, 0, seg0_data);
        segments.commit_segment(1, 1, seg1_data);
    }
    shared.condvar.notify_all();

    let result = timeout(Duration::from_secs(2), handle)
        .await
        .expect("wait_range should not hang while target data is pushed")
        .expect("wait_range task should not panic");
    assert!(matches!(result, Ok(WaitOutcome::Ready)));
    assert!(
        !shared.had_midstream_switch.load(Ordering::Acquire),
        "midstream-switch reroute must be one-shot and clear once the target range becomes ready"
    );
}

/// Budget check must return `Err(Timeout)` when waited range has no loaded
/// segments, even if total grows from entries at other offsets.
///
/// Scenario: reader waits for offset 100..200 (beyond all loaded data).
/// Background task pushes entries at 10000+ that do NOT cover the waited
/// range.  `range_ready=false` throughout.
/// Expected: budget check returns `Err(HlsError::Timeout)` after the
/// wait_range timeout expires.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn wait_range_times_out_when_total_grows_but_range_not_ready() {
    let cancel = CancellationToken::new();
    let ps = playlist_state_with_size_maps(); // 24 segs × 100 bytes = 2400 total per variant
    let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
    shared.abr_variant_index.store(0, Ordering::Release);

    let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

    // Background task: commit segments at high indices (10+) that do NOT
    // cover the waited range (100..200). This grows `total` but never
    // satisfies `range_ready` for offset range covering segment 1.
    let bg_shared = Arc::clone(&shared);
    let bg_cancel = cancel.clone();
    let bg = spawn(async move {
        for i in 0..14u64 {
            if bg_cancel.is_cancelled() {
                break;
            }
            let seg_idx = 10 + i as usize;
            let seg_data = make_segment_data(100);
            {
                let mut segments = bg_shared.segments.lock_sync();
                segments.commit_segment(0, seg_idx, seg_data);
            }
            bg_shared.condvar.notify_all();
            sleep(Duration::from_millis(200)).await;
        }
    });

    // wait_range for offset 100..200 — no entries cover this range.
    // Background entries are at 10000+. total grows but range stays unready.
    // Budget check returns Err(Timeout) after 3s.
    let handle = spawn_blocking(move || source.wait_range(100..200, Duration::from_secs(3)));

    #[cfg(not(target_arch = "wasm32"))]
    {
        let result = timeout(Duration::from_secs(10), handle)
            .await
            .expect("test must not time out")
            .expect("wait_range task should not panic");

        cancel.cancel();
        let _ = timeout(Duration::from_secs(1), bg).await;

        // Budget check returns Err(Timeout) because range is never ready.
        match result {
            Err(StreamError::Source(HlsError::Timeout(_))) => {
                // Expected: budget exceeded.
            }
            other => {
                panic!(
                    "wait_range should return Err(Timeout) when range is never ready: {other:?}"
                );
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        // On WASM, wait_range runs synchronously on the current thread,
        // so we just check that it eventually returns an error or blocks
        // until cancellation.
        cancel.cancel();
        shared.condvar.notify_all();
        let result = timeout(Duration::from_secs(5), handle)
            .await
            .expect("test must not time out")
            .expect("wait_range task should not panic");
        assert!(
            result.is_err(),
            "wait_range should return error when range is never ready: {result:?}"
        );

        let _ = timeout(Duration::from_secs(1), bg).await;
    }
}
