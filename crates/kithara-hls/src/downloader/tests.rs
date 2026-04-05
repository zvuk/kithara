use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use kithara_abr::{AbrDecision, AbrReason};
use kithara_assets::{AssetResourceState, AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{Mutex, time::Instant};
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
use kithara_stream::{AudioCodec, Downloader, PlanOutcome, Timeline};
use kithara_test_utils::kithara;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    helpers::{
        classify_layout_transition, first_missing_segment, is_cross_codec_switch, is_stale_epoch,
        should_request_init,
    },
    io::{HlsFetch, HlsPlan},
    state::HlsDownloader,
};
use crate::{
    config::HlsConfig,
    coord::SegmentRequest,
    fetch::{DefaultFetchManager, FetchManager, SegmentMeta},
    ids::SegmentId,
    parsing::{VariantId, VariantStream},
    playlist::{PlaylistAccess, PlaylistState, SegmentState, VariantSizeMap, VariantState},
    source::build_pair,
    stream_index::{SegmentData, StreamIndex},
};

#[kithara::test]
fn commit_drops_stale_fetch_epoch() {
    assert!(is_stale_epoch(7, 8));
    assert!(!is_stale_epoch(9, 9));
}

#[kithara::test]
fn first_missing_segment_detects_gap() {
    let media_url = Url::parse("https://example.com/seg.m4s").expect("valid URL");
    let mut state = StreamIndex::new(1, 3);
    state.commit_segment(
        0,
        0,
        SegmentData {
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url: media_url.clone(),
        },
    );
    state.commit_segment(
        0,
        2,
        SegmentData {
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url,
        },
    );

    assert_eq!(first_missing_segment(&state, 0, 0, 3), Some(1));
    assert_eq!(first_missing_segment(&state, 0, 2, 3), None);
    assert_eq!(first_missing_segment(&state, 0, 0, 1), None);
}

fn make_variant_state_with_segments(
    id: usize,
    codec: Option<AudioCodec>,
    segment_count: usize,
) -> VariantState {
    let base = Url::parse("https://example.com/").expect("valid base URL");
    VariantState {
        id,
        uri: base
            .join(&format!("v{id}.m3u8"))
            .expect("valid playlist URL"),
        bandwidth: Some(128_000),
        codec,
        container: None,
        init_url: None,
        segments: (0..segment_count)
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

fn make_variant_state(id: usize, codec: Option<AudioCodec>) -> VariantState {
    make_variant_state_with_segments(id, codec, 1)
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

fn test_fetch_manager_disk(cancel: CancellationToken) -> (TempDir, Arc<DefaultFetchManager>) {
    let temp_dir = TempDir::new().expect("temp dir");
    let noop_drm: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(Some("test"))
        .cache_capacity(std::num::NonZeroUsize::new(1).expect("non-zero"))
        .cancel(cancel.clone())
        .process_fn(noop_drm)
        .build();
    let net = HttpClient::new(NetOptions::default());
    (temp_dir, Arc::new(FetchManager::new(backend, net, cancel)))
}

#[kithara::test]
fn cross_codec_switch_detects_incompatible_variants() {
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, Some(AudioCodec::AacLc)),
        make_variant_state(1, Some(AudioCodec::Flac)),
    ]));
    assert!(is_cross_codec_switch(&playlist_state, 0, 1));
}

#[kithara::test]
fn cross_codec_switch_allows_same_codec_variants() {
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, Some(AudioCodec::AacLc)),
        make_variant_state(1, Some(AudioCodec::AacLc)),
    ]));
    assert!(!is_cross_codec_switch(&playlist_state, 0, 1));
}

#[kithara::test]
fn classify_same_variant_seek_is_not_midstream_switch() {
    let variant = 1;
    let segment_index = 37;

    let (is_variant_switch, is_midstream_switch) =
        classify_layout_transition(variant, variant, segment_index);

    assert!(
        !is_variant_switch,
        "seek within same variant must not trigger variant-switch init path"
    );
    assert!(
        !is_midstream_switch,
        "seek within same variant must not trigger midstream switch path"
    );
}

#[kithara::test]
fn classify_real_variant_change_marks_midstream_switch_only_after_segment_zero() {
    let from_variant = 0;
    let to_variant = 1;

    let (is_variant_switch, is_midstream_switch) =
        classify_layout_transition(from_variant, to_variant, 0);
    assert!(is_variant_switch);
    assert!(!is_midstream_switch);

    let (is_variant_switch, is_midstream_switch) =
        classify_layout_transition(from_variant, to_variant, 5);
    assert!(is_variant_switch);
    assert!(is_midstream_switch);
}

#[kithara::test(tokio)]
async fn tail_state_uses_committed_variant_for_missing_segment_rewind() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 2),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 2),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(2);
    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-0-1").expect("valid segment URL");
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }

    downloader
        .coord
        .abr_variant_index
        .store(0, Ordering::Release);
    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(200);
    assert!(
        !downloader.handle_tail_state(1, 2),
        "must exit tail state when layout != variant and new variant has missing segments"
    );
    assert_eq!(downloader.current_segment_index(), 0);
}

#[kithara::test(tokio)]
async fn populate_cached_segments_does_not_hold_stream_index_lock_during_invalidation() {
    let cancel = CancellationToken::new();
    let callback_invocations = Arc::new(AtomicUsize::new(0));
    let callback_lock_free = Arc::new(AtomicBool::new(true));
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        2,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        Arc::clone(&fetch),
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let callback_invocations_clone = Arc::clone(&callback_invocations);
    let callback_lock_free_clone = Arc::clone(&callback_lock_free);
    let segments = Arc::clone(&downloader.segments);
    let (count, cumulative_offset) = HlsDownloader::populate_cached_segments_with_open(
        &downloader.segments,
        &downloader.coord,
        playlist_state.as_ref(),
        0,
        move |_key| {
            callback_invocations_clone.fetch_add(1, Ordering::AcqRel);
            if segments.try_lock().is_err() {
                callback_lock_free_clone.store(false, Ordering::Release);
            }
            Some(AssetResourceState::Committed {
                final_len: Some(16),
            })
        },
    );

    assert_eq!(count, 2);
    assert_eq!(cumulative_offset, 32);
    assert!(
        callback_invocations.load(Ordering::Acquire) >= 2,
        "cache scan must cross the open boundary for each cached segment"
    );
    assert!(
        callback_lock_free.load(Ordering::Acquire),
        "populate_cached_segments must not hold StreamIndex lock across reentrant open callbacks"
    );
}

#[kithara::test(tokio)]
async fn populate_cached_segments_preserves_init_on_non_zero_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        4,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        Arc::clone(&fetch),
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            2,
            SegmentData {
                init_len: 44,
                media_len: 100,
                init_url: Some(
                    Url::parse("https://example.com/init-0.mp4").expect("valid init URL"),
                ),
                media_url: Url::parse("https://example.com/seg-0-2.m4s")
                    .expect("valid segment URL"),
            },
        );
    }

    let (count, _) = HlsDownloader::populate_cached_segments_with_open(
        &downloader.segments,
        &downloader.coord,
        playlist_state.as_ref(),
        0,
        |_key| {
            Some(AssetResourceState::Committed {
                final_len: Some(100),
            })
        },
    );

    assert_eq!(count, 4, "all segments must be populated");

    let seg2_init_len = downloader
        .segments
        .lock_sync()
        .stored_segment(0, 2)
        .expect("segment 2 must exist")
        .init_len;
    assert_eq!(
        seg2_init_len, 44,
        "populate_cached_segments must preserve init_len on a segment \
         that was committed with init data during a midstream switch"
    );
}

#[kithara::test(tokio)]
async fn populate_cached_segments_ignores_active_disk_resource() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        1,
    )]));
    let (_temp_dir, fetch) = test_fetch_manager_disk(cancel);

    let media_url = playlist_state
        .segment_url(0, 0)
        .expect("segment URL present");
    let key = ResourceKey::from_url(&media_url);
    let active = fetch
        .backend()
        .acquire_resource_with_ctx(&key, None)
        .expect("acquire active resource");
    active.write_at(0, &[1; 16]).expect("write active bytes");

    let evict_key = ResourceKey::from_url(
        &Url::parse("https://example.com/seg-evict-0.m4s").expect("valid evict URL"),
    );
    let evict = fetch
        .backend()
        .acquire_resource_with_ctx(&evict_key, None)
        .expect("acquire evicting resource");
    evict.write_at(0, &[2; 16]).expect("write evicting bytes");

    assert_eq!(
        fetch
            .backend()
            .resource_state(&key)
            .expect("resource state"),
        AssetResourceState::Active
    );

    let segments = Mutex::new(StreamIndex::new(1, 1));
    let coord = crate::coord::HlsCoord::new(
        CancellationToken::new(),
        Timeline::new(),
        Arc::new(AtomicUsize::new(0)),
    );

    let (count, cumulative_offset) = HlsDownloader::populate_cached_segments(
        &segments,
        &coord,
        fetch.as_ref(),
        playlist_state.as_ref(),
        0,
    );

    assert_eq!(
        count, 0,
        "active disk resource must not be treated as a cached committed segment"
    );
    assert_eq!(cumulative_offset, 0);
    assert!(
        segments.lock_sync().find_at_offset(0).is_none(),
        "active disk resource must not be materialized into StreamIndex"
    );
}

#[kithara::test(tokio)]
async fn populate_cached_segments_if_needed_scans_on_variant_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        2,
    )]));
    let variants = parsed_variants(1);
    let (_temp_dir, fetch) = test_fetch_manager_disk(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        Arc::clone(&fetch),
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = playlist_state
        .segment_url(0, 0)
        .expect("segment URL present");
    let key = ResourceKey::from_url(&media_url);
    let resource = fetch
        .backend()
        .acquire_resource_with_ctx(&key, None)
        .expect("acquire resource");
    resource.write_at(0, &[0xAA; 64]).expect("write bytes");
    resource.commit(Some(64)).expect("commit resource");

    let (cached_count, _cached_end_offset) = downloader.populate_cached_segments_if_needed(0);

    assert!(
        cached_count > 0,
        "populate_cached_segments_if_needed must scan disk cache on variant switch, \
         but Bug C early return produces cached_count=0"
    );
}

#[kithara::test(tokio)]
async fn multi_switch_v0_v1_v0_cursor_converges() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 5),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 5),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        for seg_idx in 0..5 {
            segments.commit_segment(
                0,
                seg_idx,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/seg-0-{seg_idx}.m4s"))
                        .expect("valid URL"),
                },
            );
        }
        drop(segments);
    }

    downloader.handle_midstream_switch(true);
    assert_eq!(
        downloader.current_segment_index(),
        5,
        "after V0->V1 midstream switch, cursor must be at first_missing(V0)=5"
    );

    downloader.download_variant = 1;

    let (cached_count_v1, cached_offset_v1) = HlsDownloader::populate_cached_segments_with_open(
        &downloader.segments,
        &downloader.coord,
        playlist_state.as_ref(),
        1,
        |_key| None,
    );
    assert_eq!(cached_count_v1, 0);
    downloader.apply_cached_segment_progress(1, cached_count_v1, cached_offset_v1);

    assert_eq!(downloader.current_segment_index(), 5);
    assert_eq!(downloader.download_variant, 1);

    downloader.handle_midstream_switch(true);
    assert_eq!(
        downloader.current_segment_index(),
        0,
        "after V1->V0 midstream switch, cursor must be at first_missing(V1)=0"
    );

    downloader.download_variant = 0;

    let (cached_count_v0, cached_offset_v0) = HlsDownloader::populate_cached_segments_with_open(
        &downloader.segments,
        &downloader.coord,
        playlist_state.as_ref(),
        0,
        |_key| {
            Some(AssetResourceState::Committed {
                final_len: Some(100),
            })
        },
    );
    assert_eq!(
        cached_count_v0, 5,
        "cache scan must find all 5 committed V0 segments"
    );

    downloader.apply_cached_segment_progress(0, cached_count_v0, cached_offset_v0);

    assert_eq!(
        downloader.download_variant, 0,
        "download_variant must converge to V0 after V0->V1->V0 multi-switch"
    );
    assert_eq!(
        downloader.current_segment_index(),
        5,
        "cursor must converge to 5 after V0->V1->V0 multi-switch \
         (cache scan advances past all committed V0 segments)"
    );
}

#[kithara::test(tokio)]
async fn tail_state_keeps_eof_false_when_playback_not_at_end() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        2,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(2);
    downloader
        .coord
        .abr_variant_index
        .store(0, Ordering::Release);
    downloader.coord.timeline().set_byte_position(0);
    downloader.coord.timeline().set_eof(false);
    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }

    assert!(downloader.handle_tail_state(0, 2));
    assert!(
        !downloader.coord.timeline().eof(),
        "downloader must not set eof while playback position is not at stream end"
    );
}

#[kithara::test(tokio)]
async fn should_not_throttle_when_current_variant_has_gap_to_fill() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.look_ahead_bytes = Some(100);
    downloader.look_ahead_segments = Some(1);
    downloader.cursor.reset_fill(0);
    downloader
        .coord
        .abr_variant_index
        .store(1, Ordering::Release);
    downloader.coord.timeline().set_byte_position(0);
    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-0").expect("valid URL"),
            },
        );
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-1").expect("valid URL"),
            },
        );
        segments.commit_segment(
            0,
            2,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-2").expect("valid URL"),
            },
        );
    }

    assert!(
        !downloader.should_throttle(),
        "downloader must not throttle while the current variant still has a missing gap"
    );
}

#[kithara::test(tokio)]
async fn tail_state_ignores_stale_committed_variant() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(3);
    downloader
        .coord
        .abr_variant_index
        .store(0, Ordering::Release);
    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            0,
            2,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(300);
    assert!(
        !downloader.handle_tail_state(1, 3),
        "must exit tail state to download new variant segments"
    );
    assert_eq!(downloader.current_segment_index(), 0);
}

#[kithara::test(tokio)]
async fn tail_state_does_not_rewind_to_uncommitted_variant() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 2),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 2),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(2);
    downloader
        .coord
        .abr_variant_index
        .store(1, Ordering::Release);
    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(200);
    assert!(
        !downloader.handle_tail_state(1, 2),
        "must exit tail state to download new variant segments"
    );
    assert_eq!(downloader.current_segment_index(), 0);
}

#[kithara::test(tokio)]
async fn tail_state_does_not_rewind_from_tail_when_playback_not_at_end() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(3);
    downloader
        .coord
        .abr_variant_index
        .store(1, Ordering::Release);
    downloader
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);
    {
        let mut segments = downloader.segments.lock_sync();
        segments.set_layout_variant(1);
        let media_url = Url::parse("https://example.com/seg-1-0").expect("valid segment URL");
        segments.commit_segment(
            1,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            1,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        segments.commit_segment(
            1,
            2,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }
    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(150);

    assert!(downloader.handle_tail_state(1, 3));
    assert!(
        !downloader.coord.timeline().eof(),
        "tail handler must not force EOF while playback is not at stream end"
    );
    assert_eq!(downloader.current_segment_index(), 3);
}

#[kithara::test(tokio)]
async fn tail_state_does_not_rewind_unseen_variant_without_midstream_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reopen_fill(0, 3);
    downloader
        .coord
        .abr_variant_index
        .store(1, Ordering::Release);
    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(0);

    {
        let mut segments = downloader.segments.lock_sync();
        for segment_index in 0..3 {
            segments.commit_segment(
                0,
                segment_index,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/seg-0-{segment_index}"))
                        .expect("valid segment URL"),
                },
            );
        }
        drop(segments);
    }

    assert!(
        !downloader.handle_tail_state(1, 3),
        "must exit tail state to download new variant segments"
    );
    assert_eq!(
        downloader.current_segment_index(),
        0,
        "cursor must rewind to first missing segment of new variant"
    );
}

#[kithara::test(tokio)]
async fn plan_while_flushing_does_not_mark_eof() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
        0,
        Some(AudioCodec::AacLc),
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(1);
    downloader.coord.timeline().set_eof(false);
    let _ = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(1));
    assert!(downloader.coord.timeline().is_flushing());

    let outcome = Downloader::plan(&mut downloader).await;
    assert!(matches!(outcome, PlanOutcome::Idle));
    assert!(
        !downloader.coord.timeline().eof(),
        "plan must not set EOF while seek flushing is active"
    );
}

#[kithara::test(tokio)]
async fn single_segment_playlist_plans_only_segment_zero() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
        0,
        Some(AudioCodec::AacLc),
        1,
        100,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let outcome = Downloader::plan(&mut downloader).await;
    let PlanOutcome::Batch(plans) = outcome else {
        panic!("single-segment playlist must plan exactly one batch item");
    };
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].variant, 0);
    assert_eq!(plans[0].segment, SegmentId::Media(0));
    assert!(
        !plans[0].need_init,
        "single-segment init-less playlist must not request synthetic init"
    );
}

#[kithara::test(tokio)]
async fn single_segment_playlist_reaches_tail_after_commit() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
        0,
        Some(AudioCodec::AacLc),
        1,
        100,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.commit(HlsFetch {
        init_len: 0,
        init_url: None,
        media: SegmentMeta {
            variant: 0,
            segment_type: crate::fetch::SegmentType::Media(0),
            sequence: 0,
            url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid URL"),
            duration: None,
            key: None,
            len: 100,
            container: None,
        },
        media_cached: false,
        segment: SegmentId::Media(0),
        variant: 0,
        duration: Duration::from_millis(10),
        seek_epoch: 0,
    });

    assert_eq!(downloader.current_segment_index(), 1);
    downloader.coord.timeline().set_byte_position(100);

    let outcome = Downloader::plan(&mut downloader).await;
    assert!(matches!(outcome, PlanOutcome::Idle));
    assert!(
        downloader.coord.timeline().eof(),
        "single-segment playlist must reach EOF without any rewind or special-case path"
    );
}

#[kithara::test(tokio)]
async fn plan_with_new_seek_epoch_does_not_mark_eof_from_stale_tail() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
        0,
        Some(AudioCodec::AacLc),
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(1);
    downloader.coord.timeline().set_eof(false);

    let epoch = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(1));
    downloader.coord.timeline().complete_seek(epoch);
    assert!(!downloader.coord.timeline().is_flushing());
    assert_ne!(
        downloader.coord.timeline().seek_epoch(),
        downloader.active_seek_epoch
    );

    let outcome = Downloader::plan(&mut downloader).await;
    assert!(matches!(outcome, PlanOutcome::Idle));
    assert!(
        !downloader.coord.timeline().eof(),
        "plan must not emit EOF while seek epoch is newer than downloader state"
    );
}

#[kithara::test(tokio)]
async fn plan_while_seek_pending_is_idle() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
        0,
        Some(AudioCodec::AacLc),
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reset_fill(1);
    let epoch = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(1));
    downloader.coord.timeline().complete_seek(epoch);

    assert!(
        downloader.coord.timeline().is_seek_pending(),
        "seek_pending must stay set until decoder applies the seek"
    );
    assert!(
        !downloader.coord.timeline().is_flushing(),
        "complete_seek clears flushing before decoder applies seek"
    );

    let outcome = Downloader::plan(&mut downloader).await;
    assert!(matches!(outcome, PlanOutcome::Idle));
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_keeps_init_markers_on_known_variant() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
        0,
        Some(AudioCodec::AacLc),
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.sent_init_for_variant.insert(0);

    downloader.reset_for_seek_epoch(1, 0, 0);

    assert!(
        downloader.sent_init_for_variant.contains(&0),
        "known variant should keep init marker after seek reset"
    );
    assert!(
        !downloader.force_init_for_seek,
        "same-codec seek reset should not force init"
    );
    assert_eq!(downloader.abr.get_current_variant_index(), 0);
    assert_eq!(downloader.gap_scan_start_segment(), 0);
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_to_unseen_variant_sets_new_baseline() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, Some(AudioCodec::AacLc)),
        make_variant_state(1, Some(AudioCodec::AacLc)),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.sent_init_for_variant.insert(0);

    downloader.reset_for_seek_epoch(2, 1, 0);

    assert!(
        downloader.sent_init_for_variant.contains(&0),
        "known variant markers must be preserved on reset"
    );
    assert!(
        !downloader.sent_init_for_variant.contains(&1),
        "unseen variant must remain without init marker"
    );
    assert!(
        !downloader.force_init_for_seek,
        "same-codec seek to unseen variant should not force init"
    );
    assert_eq!(downloader.abr.get_current_variant_index(), 1);
    assert_eq!(downloader.gap_scan_start_segment(), 0);
}

#[kithara::test(tokio)]
async fn build_demand_plan_same_codec_variant_change_does_not_request_init() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, Some(AudioCodec::AacLc)),
        make_variant_state(1, Some(AudioCodec::AacLc)),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.sent_init_for_variant.insert(0);

    let (is_variant_switch, _is_midstream_switch) = downloader.classify_variant_transition(1, 5);
    let plan = downloader.build_demand_plan(
        &SegmentRequest {
            segment_index: 5,
            variant: 1,
            seek_epoch: 0,
        },
        is_variant_switch,
    );

    assert!(
        !plan.need_init,
        "same-codec variant change must not inject init into demand plan"
    );
}

#[kithara::test(tokio)]
async fn build_demand_plan_initial_nonzero_segment_without_init_does_not_request_init() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        4,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let (is_variant_switch, _is_midstream_switch) = downloader.classify_variant_transition(0, 2);
    let plan = downloader.build_demand_plan(
        &SegmentRequest {
            segment_index: 2,
            variant: 0,
            seek_epoch: 0,
        },
        is_variant_switch,
    );

    assert!(
        !plan.need_init,
        "fresh direct demand on init-less playlist must not inject init"
    );
}

#[kithara::test(tokio)]
async fn build_demand_plan_initial_segment_zero_without_init_does_not_request_init() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        4,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let (is_variant_switch, _is_midstream_switch) = downloader.classify_variant_transition(0, 0);
    let plan = downloader.build_demand_plan(
        &SegmentRequest {
            segment_index: 0,
            variant: 0,
            seek_epoch: 0,
        },
        is_variant_switch,
    );

    assert!(
        !plan.need_init,
        "init-less playlist must not request init on initial segment zero"
    );
}

#[kithara::test(tokio)]
async fn build_batch_plans_same_codec_variant_change_keeps_metadata_layout() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.sent_init_for_variant.insert(0);
    downloader.cursor.reset_fill(5);
    downloader.prefetch_count = 2;
    let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(1, 5);
    let (plans, _batch_end) = downloader.build_batch_plans(
        1,
        10,
        is_variant_switch,
        is_midstream_switch,
        Some(0),
        false,
    );

    assert!(
        !plans.is_empty(),
        "same-codec variant change should still build sequential plans"
    );
    assert!(
        !plans[0].need_init,
        "same-codec variant change must not inject init into batch plan"
    );
}

#[kithara::test(tokio)]
async fn build_batch_plans_seek_into_init_less_playlist_starts_at_target_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        6,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.prefetch_count = 2;
    downloader.reset_for_seek_epoch(1, 0, 2);
    let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(0, 2);
    let (plans, _batch_end) =
        downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch, None, false);

    assert_eq!(
        plans.first().map(|p| p.segment),
        Some(SegmentId::Media(2)),
        "seek batch must start from the requested segment"
    );
    assert!(
        plans.iter().all(|plan| !plan.need_init),
        "seek batch on init-less playlist must not inject init"
    );
}

#[kithara::test(tokio)]
async fn build_batch_plans_initial_segment_zero_without_init_does_not_request_init() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        6,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.prefetch_count = 2;
    let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(0, 0);
    let (plans, _batch_end) =
        downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch, None, false);

    assert_eq!(
        plans.first().map(|p| p.segment),
        Some(SegmentId::Media(0)),
        "initial batch must start from segment zero"
    );
    assert!(
        plans.iter().all(|plan| !plan.need_init),
        "init-less playlist must not inject init into initial batch"
    );
}

#[kithara::test(tokio)]
async fn build_batch_plans_suppresses_prefetch_immediately_after_seek() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        10,
    )]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            init_size: 0,
            segment_sizes: vec![100; 10],
            offsets: (0u64..10).map(|index| index * 100).collect(),
            total: 1_000,
        },
    );
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let epoch = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(1));
    downloader.coord.timeline().complete_seek(epoch);
    downloader.reset_for_seek_epoch(epoch, 0, 5);
    downloader.cursor.reopen_fill(5, 6);
    downloader.prefetch_count = 3;
    downloader.coord.timeline().set_byte_position(500);
    let (plans, batch_end) = downloader.build_batch_plans(0, 10, false, false, None, false);
    assert!(
        plans.is_empty(),
        "post-seek planner must wait for demand-driven resume"
    );
    assert_eq!(batch_end, 6);
}

#[kithara::test(tokio)]
async fn build_batch_plans_limits_parallel_prefetch_during_active_seek_epoch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::Flac),
        20,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let epoch = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(67));
    downloader.coord.timeline().complete_seek(epoch);
    downloader.reset_for_seek_epoch(epoch, 0, 11);
    downloader.cursor.reset_fill(12);
    downloader.prefetch_count = 3;
    downloader.coord.timeline().set_byte_position(8_451_629);
    let (plans, batch_end) = downloader.build_batch_plans(0, 20, false, false, None, false);
    assert_eq!(
        plans.len(),
        1,
        "active seek epoch must not launch multiple parallel prefetches"
    );
    assert_eq!(plans[0].segment, SegmentId::Media(12));
    assert_eq!(batch_end, 13);
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_forces_init_on_cross_codec_seek() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state(0, Some(AudioCodec::AacLc)),
        make_variant_state(1, Some(AudioCodec::Flac)),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.reset_for_seek_epoch(3, 1, 2);

    assert!(
        downloader.force_init_for_seek,
        "cross-codec seek reset must force init for the first segment"
    );
    assert_eq!(downloader.abr.get_current_variant_index(), 1);
    assert_eq!(downloader.gap_scan_start_segment(), 2);
}

#[kithara::test(tokio)]
async fn commit_preserves_init_for_first_cross_codec_seek_target_segment() {
    let cancel = CancellationToken::new();
    let base = Url::parse("https://example.com/").expect("valid base URL");
    let init_url = base.join("init-1.mp4").expect("valid init URL");
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_size_map(0, Some(AudioCodec::AacLc), 6, 100),
        VariantState {
            id: 1,
            uri: base.join("v1.m3u8").expect("valid playlist URL"),
            bandwidth: Some(128_000),
            codec: Some(AudioCodec::Flac),
            container: None,
            init_url: Some(init_url.clone()),
            segments: (0..6)
                .map(|index| SegmentState {
                    index,
                    url: base
                        .join(&format!("seg-1-{index}.m4s"))
                        .expect("valid segment URL"),
                    duration: Duration::from_secs(4),
                    key: None,
                })
                .collect(),
            size_map: Some(VariantSizeMap {
                init_size: 20,
                offsets: vec![0, 120, 220, 320, 420, 520],
                segment_sizes: vec![120, 100, 100, 100, 100, 100],
                total: 620,
            }),
        },
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let seek_epoch = downloader
        .coord
        .timeline()
        .initiate_seek(Duration::from_secs(12));
    downloader.reset_for_seek_epoch(seek_epoch, 1, 3);
    downloader.segments.lock_sync().set_layout_variant(1);

    let (is_variant_switch, _is_midstream_switch) = downloader.classify_variant_transition(1, 3);
    assert!(
        !is_variant_switch,
        "after seek reset the layout already points at the target variant, so this path must not depend on switch detection"
    );

    let plan = downloader.build_demand_plan(
        &SegmentRequest {
            segment_index: 3,
            variant: 1,
            seek_epoch,
        },
        is_variant_switch,
    );
    assert!(
        plan.need_init,
        "cross-codec seek reset must still request init for the first decoder-visible segment"
    );

    downloader.commit(HlsFetch {
        init_len: 20,
        init_url: Some(init_url),
        media: SegmentMeta {
            variant: 1,
            segment_type: crate::fetch::SegmentType::Media(3),
            sequence: 3,
            url: base.join("seg-1-3.m4s").expect("valid segment URL"),
            duration: None,
            key: None,
            len: 100,
            container: None,
        },
        media_cached: false,
        segment: SegmentId::Media(3),
        variant: 1,
        duration: Duration::from_millis(1),
        seek_epoch,
    });

    let init_len = downloader
        .segments
        .lock_sync()
        .stored_segment(1, 3)
        .expect("target segment must be committed")
        .init_len;
    assert_eq!(
        init_len, 20,
        "the first decoder-visible segment after a cross-codec seek reset must retain init bytes even when the layout already points at the target variant"
    );
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_preserves_effective_total_without_timeline_cache() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        3,
    )]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            init_size: 0,
            offsets: vec![0, 800, 1_600],
            segment_sizes: vec![800, 800, 800],
            total: 2_400,
        },
    );
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    assert_eq!(downloader.effective_total_bytes(), Some(2_400));

    downloader.reset_for_seek_epoch(1, 0, 0);

    assert_eq!(
        downloader.effective_total_bytes(),
        Some(2_400),
        "seek reset must preserve HLS byte length through StreamIndex metadata"
    );
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_same_variant_preserves_effective_total() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::Flac),
        3,
    )]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            init_size: 20,
            offsets: vec![0, 120, 220],
            segment_sizes: vec![120, 100, 100],
            total: 320,
        },
    );
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 20,
                media_len: 220,
                init_url: None,
                media_url,
            },
        );
    }

    assert_eq!(downloader.effective_total_bytes(), Some(440));

    downloader.reset_for_seek_epoch(1, 0, 1);

    assert_eq!(
        downloader.effective_total_bytes(),
        Some(440),
        "same-variant seek reset must preserve the existing effective total"
    );
}

#[kithara::test(tokio)]
async fn effective_total_bytes_derives_total_from_stream_index() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
    ]));
    playlist_state.set_size_map(
        1,
        VariantSizeMap {
            init_size: 20,
            offsets: vec![0, 120, 220],
            segment_sizes: vec![120, 100, 100],
            total: 320,
        },
    );
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.segments.lock_sync().set_layout_variant(1);

    assert_eq!(
        downloader.effective_total_bytes(),
        Some(320),
        "effective total must be derived from StreamIndex (estimated variant 1 sizes)"
    );
}

fn make_variant_state_with_size_map(
    id: usize,
    codec: Option<AudioCodec>,
    segment_count: usize,
    seg_size: u64,
) -> VariantState {
    let base = Url::parse("https://example.com/").expect("valid base URL");
    let offsets: Vec<u64> = (0..segment_count).map(|i| i as u64 * seg_size).collect();
    let total = segment_count as u64 * seg_size;
    VariantState {
        id,
        uri: base
            .join(&format!("v{id}.m3u8"))
            .expect("valid playlist URL"),
        bandwidth: Some(128_000),
        codec,
        container: None,
        init_url: None,
        segments: (0..segment_count)
            .map(|index| SegmentState {
                index,
                url: base
                    .join(&format!("seg-{id}-{index}.m4s"))
                    .expect("valid segment URL"),
                duration: Duration::from_secs(4),
                key: None,
            })
            .collect(),
        size_map: Some(VariantSizeMap {
            init_size: 0,
            segment_sizes: vec![seg_size; segment_count],
            offsets,
            total,
        }),
    }
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_same_variant_keeps_watermark() {
    let cancel = CancellationToken::new();
    let seg_size = 1000;
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
        0,
        Some(AudioCodec::AacLc),
        10,
        seg_size,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.coord.timeline().set_download_position(5000);

    downloader.reset_for_seek_epoch(1, 0, 2);

    assert_eq!(
        downloader.coord.timeline().download_position(),
        5000,
        "same-variant seek must keep download_position at committed watermark"
    );
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_same_variant_forward_raises_watermark() {
    let cancel = CancellationToken::new();
    let seg_size = 1000;
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
        0,
        Some(AudioCodec::AacLc),
        10,
        seg_size,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.coord.timeline().set_download_position(5000);

    downloader.reset_for_seek_epoch(1, 0, 7);

    assert_eq!(
        downloader.coord.timeline().download_position(),
        7000,
        "forward same-variant seek must raise download_position to target"
    );
}

#[kithara::test(tokio)]
async fn reset_for_seek_epoch_cross_variant_resets_position() {
    let cancel = CancellationToken::new();
    let seg_size = 1000;
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_size_map(0, Some(AudioCodec::AacLc), 10, seg_size),
        make_variant_state_with_size_map(1, Some(AudioCodec::Flac), 10, seg_size),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.coord.timeline().set_download_position(5000);

    downloader.reset_for_seek_epoch(2, 1, 3);

    assert_eq!(
        downloader.coord.timeline().download_position(),
        3000,
        "cross-variant seek must reset download_position to target metadata offset"
    );
}

#[kithara::test]
fn should_request_init_for_segment_zero_even_when_variant_is_known() {
    assert!(should_request_init(false, SegmentId::Media(0)));
    assert!(!should_request_init(false, SegmentId::Media(1)));
}

#[kithara::test]
fn should_request_init_only_for_segment_zero_or_variant_switch() {
    assert!(!should_request_init(false, SegmentId::Media(5)));
    assert!(should_request_init(true, SegmentId::Media(5)));
}

#[kithara::test(tokio)]
async fn commit_drops_old_cross_codec_fetch_after_switched_anchor_is_committed() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
    ]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            init_size: 0,
            offsets: vec![0, 100, 200],
            segment_sizes: vec![100, 100, 100],
            total: 300,
        },
    );
    playlist_state.set_size_map(
        1,
        VariantSizeMap {
            init_size: 20,
            offsets: vec![0, 120, 220],
            segment_sizes: vec![120, 100, 100],
            total: 320,
        },
    );

    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.abr.apply(
        &AbrDecision {
            target_variant_index: 1,
            reason: AbrReason::DownSwitch,
            changed: true,
        },
        Instant::now(),
    );
    downloader.coord.timeline().set_download_position(220);
    {
        let mut segments = downloader.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid URL"),
            },
        );
        segments.commit_segment(
            1,
            1,
            SegmentData {
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
            },
        );
    }

    downloader.commit(HlsFetch {
        init_len: 0,
        init_url: None,
        media: SegmentMeta {
            variant: 0,
            segment_type: crate::fetch::SegmentType::Media(2),
            sequence: 2,
            url: Url::parse("https://example.com/seg-0-2.m4s").expect("valid URL"),
            duration: None,
            key: None,
            len: 100,
            container: None,
        },
        media_cached: false,
        segment: SegmentId::Media(2),
        variant: 0,
        duration: Duration::from_millis(1),
        seek_epoch: 0,
    });

    let is_loaded = downloader.segments.lock_sync().is_segment_loaded(0, 2);
    assert!(
        !is_loaded,
        "old cross-codec fetch must not re-enter the switched layout after a new anchor is committed"
    );
    assert_eq!(downloader.abr.get_current_variant_index(), 1);
}

#[kithara::test(tokio)]
async fn commit_keeps_first_target_cross_codec_fetch_before_new_anchor_commits() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 6),
        make_variant_state_with_segments(1, Some(AudioCodec::Flac), 6),
    ]));
    playlist_state.set_size_map(
        0,
        VariantSizeMap {
            init_size: 0,
            offsets: vec![0, 100, 200, 300, 400, 500],
            segment_sizes: vec![100, 100, 100, 100, 100, 100],
            total: 600,
        },
    );
    playlist_state.set_size_map(
        1,
        VariantSizeMap {
            init_size: 20,
            offsets: vec![0, 120, 220, 320, 420, 520],
            segment_sizes: vec![120, 100, 100, 100, 100, 100],
            total: 620,
        },
    );

    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.abr.apply(
        &AbrDecision {
            target_variant_index: 1,
            reason: AbrReason::DownSwitch,
            changed: true,
        },
        Instant::now(),
    );
    downloader.cursor.reset_fill(3);
    downloader
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);
    downloader.coord.timeline().set_download_position(300);
    {
        let mut segments = downloader.segments.lock_sync();
        if let Some(sizes) = playlist_state.segment_sizes(1) {
            segments.set_expected_sizes(1, sizes);
        }
        for segment_index in 0..3 {
            segments.commit_segment(
                0,
                segment_index,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!(
                        "https://example.com/seg-0-{segment_index}.m4s",
                    ))
                    .expect("valid URL"),
                },
            );
        }
        drop(segments);
    }

    downloader.commit(HlsFetch {
        init_len: 20,
        init_url: Some(Url::parse("https://example.com/init-1.mp4").expect("valid URL")),
        media: SegmentMeta {
            variant: 1,
            segment_type: crate::fetch::SegmentType::Media(3),
            sequence: 3,
            url: Url::parse("https://example.com/seg-1-3.m4s").expect("valid URL"),
            duration: None,
            key: None,
            len: 100,
            container: None,
        },
        media_cached: false,
        segment: SegmentId::Media(3),
        variant: 1,
        duration: Duration::from_millis(1),
        seek_epoch: 0,
    });

    let segments = downloader.segments.lock_sync();
    assert!(
        segments.is_segment_loaded(1, 3),
        "first target cross-codec fetch must commit"
    );
    let range = <StreamIndex as kithara_stream::LayoutIndex>::item_range(&segments, (1, 3))
        .expect("committed segment must have byte range");
    assert_eq!(
        range.start, 320,
        "first switched segment must appear at the correct offset in variant 1's byte space"
    );
    drop(segments);
    assert_eq!(downloader.current_segment_index(), 4);
    assert_eq!(downloader.abr.get_current_variant_index(), 1);
}

#[kithara::test(tokio)]
async fn poll_demand_drops_same_variant_request_below_switch_floor() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.abr.apply(
        &AbrDecision {
            target_variant_index: 1,
            reason: AbrReason::DownSwitch,
            changed: true,
        },
        Instant::now(),
    );
    downloader.cursor.reopen_fill(4, 4);
    downloader
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);
    downloader.coord.requeue_segment_request(SegmentRequest {
        segment_index: 2,
        variant: 1,
        seek_epoch: downloader.coord.timeline().seek_epoch(),
    });

    let plan = downloader.poll_demand().await;
    assert!(plan.is_none(), "demand below switch floor must be dropped");
}

#[kithara::test(tokio)]
async fn commit_drops_same_variant_fetch_below_switch_floor() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };
    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.abr.apply(
        &AbrDecision {
            target_variant_index: 1,
            reason: AbrReason::DownSwitch,
            changed: true,
        },
        Instant::now(),
    );
    downloader.cursor.reopen_fill(4, 4);
    downloader
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);

    downloader.commit(HlsFetch {
        init_len: 44,
        init_url: Some(Url::parse("https://example.com/init-1.mp4").expect("valid URL")),
        media: SegmentMeta {
            variant: 1,
            segment_type: crate::fetch::SegmentType::Media(0),
            sequence: 0,
            url: Url::parse("https://example.com/seg-1-0.m4s").expect("valid URL"),
            duration: None,
            key: None,
            len: 100,
            container: None,
        },
        media_cached: false,
        segment: SegmentId::Media(0),
        variant: 1,
        duration: Duration::from_millis(1),
        seek_epoch: 0,
    });

    let segments = downloader.segments.lock_sync();
    assert!(
        !segments.is_segment_loaded(1, 0),
        "fetch below switch floor must not enter switched layout"
    );
    drop(segments);
    assert_eq!(downloader.current_segment_index(), 4);
}

#[kithara::test(tokio)]
async fn handle_midstream_switch_notifies_condvar_for_repush() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        for seg_idx in 0..2 {
            segments.commit_segment(
                0,
                seg_idx,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/v0-seg-{seg_idx}.m4s",))
                        .expect("valid URL"),
                },
            );
        }
        drop(segments);
    }

    downloader.cursor.reset_fill(4);

    downloader.coord.requeue_segment_request(SegmentRequest {
        segment_index: 1,
        variant: 1,
        seek_epoch: 0,
    });

    assert!(
        !downloader
            .coord
            .had_midstream_switch
            .load(Ordering::Acquire)
    );

    downloader.handle_midstream_switch(true);

    assert!(
        downloader.coord.take_segment_request().is_none(),
        "handle_midstream_switch must drain all requests"
    );

    assert_eq!(
        downloader.current_segment_index(),
        2,
        "midstream switch must advance cursor to first missing segment"
    );

    assert!(
        downloader
            .coord
            .had_midstream_switch
            .load(Ordering::Acquire),
        "had_midstream_switch must be true after handle_midstream_switch(true)"
    );
}

#[kithara::test]
fn cursor_advances_to_first_missing_after_midstream_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        for seg_idx in 0..2 {
            segments.commit_segment(
                0,
                seg_idx,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/v0-seg-{seg_idx}.m4s",))
                        .expect("valid URL"),
                },
            );
        }
        drop(segments);
    }

    downloader.handle_midstream_switch(true);

    assert_eq!(
        downloader.current_segment_index(),
        2,
        "midstream switch must advance cursor to first missing segment in old variant"
    );
    assert_eq!(
        downloader.gap_scan_start_segment(),
        2,
        "midstream switch must advance floor to first missing segment"
    );
}

#[kithara::test]
fn cursor_falls_back_to_zero_when_no_committed_segments() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.handle_midstream_switch(true);

    assert_eq!(
        downloader.current_segment_index(),
        0,
        "midstream switch with no committed segments must fall back to cursor 0"
    );
}

#[kithara::test]
fn cursor_advances_to_num_segments_when_all_loaded() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    {
        let mut segments = downloader.segments.lock_sync();
        for seg_idx in 0..3 {
            segments.commit_segment(
                0,
                seg_idx,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/v0-seg-{seg_idx}.m4s",))
                        .expect("valid URL"),
                },
            );
        }
        drop(segments);
    }

    downloader.handle_midstream_switch(true);

    assert_eq!(
        downloader.current_segment_index(),
        3,
        "midstream switch with all segments loaded must advance cursor to num_segments"
    );
}

#[kithara::test]
fn had_midstream_switch_is_true_after_handle_midstream_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    assert!(
        !downloader
            .coord
            .had_midstream_switch
            .load(Ordering::Acquire),
        "had_midstream_switch must be false before call"
    );

    downloader.handle_midstream_switch(true);

    assert!(
        downloader
            .coord
            .had_midstream_switch
            .load(Ordering::Acquire),
        "had_midstream_switch must be true after handle_midstream_switch(true)"
    );
}

#[kithara::test(tokio)]
async fn tail_state_rewinds_missing_segments_after_midstream_switch() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    downloader.cursor.reopen_fill(1, 3);
    downloader
        .coord
        .abr_variant_index
        .store(1, Ordering::Release);
    downloader
        .coord
        .had_midstream_switch
        .store(true, Ordering::Release);

    {
        let mut segments = downloader.segments.lock_sync();
        let media_url = Url::parse("https://example.com/seg-1-1").expect("valid segment URL");
        segments.commit_segment(
            1,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.coord.timeline().set_eof(false);
    downloader.coord.timeline().set_byte_position(150);

    assert!(downloader.handle_tail_state(1, 3));
    assert_eq!(
        downloader.current_segment_index(),
        2,
        "tail handler must rewind to the first missing segment after the switch point"
    );
    assert!(
        !downloader.coord.timeline().eof(),
        "gap rewind after midstream switch must not force EOF"
    );
}

#[kithara::test]
fn segment_loaded_for_demand_tracks_resource_presence_in_ephemeral_mode() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        10,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let init_url = Url::parse("https://example.com/init-0.mp4").expect("valid init URL");
    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");

    {
        let init_key = ResourceKey::from_url(&init_url);
        let init_res = downloader
            .fetch
            .backend()
            .acquire_resource(&init_key)
            .expect("open init resource");
        init_res.write_at(0, b"init_data").expect("write init");
        init_res.commit(None).expect("commit init");

        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("open media resource");
        media_res.write_at(0, b"media_data").expect("write media");
        media_res.commit(None).expect("commit media");
    }

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 9,
                media_len: 10,
                init_url: Some(init_url),
                media_url,
            },
        );
    }

    assert!(
        downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
        "segment_loaded_for_demand must return true when segment resources are present"
    );

    for i in 1..20 {
        let key = ResourceKey::new(format!("evict-{i}.m4s"));
        let res = downloader
            .fetch
            .backend()
            .acquire_resource(&key)
            .expect("open evict resource");
        res.write_at(0, b"x").expect("write");
        res.commit(None).expect("commit");
    }

    assert!(
        !downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
        "segment_loaded_for_demand must return false when init resource is evicted"
    );
}

#[kithara::test]
fn segment_loaded_for_demand_requires_committed_resource_in_ephemeral_mode() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
        0,
        Some(AudioCodec::AacLc),
        10,
    )]));
    let variants = parsed_variants(1);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = downloader
        .fetch
        .backend()
        .acquire_resource(&media_key)
        .expect("open media resource");
    media_res.write_at(0, b"media_data").expect("write media");

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 10,
                init_url: None,
                media_url,
            },
        );
    }

    assert!(
        !downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
        "segment_loaded_for_demand must not treat active resources as already loaded"
    );
}

#[kithara::test]
fn segment_loaded_for_demand_requires_visible_layout_entry() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = Url::parse("https://example.com/hidden-seg-0-1.m4s").expect("valid media URL");
    let media_key = ResourceKey::from_url(&media_url);
    let media_res = downloader
        .fetch
        .backend()
        .acquire_resource(&media_key)
        .expect("open media resource");
    media_res.write_at(0, b"hidden_media").expect("write media");
    media_res.commit(Some(12)).expect("commit media");

    {
        let mut segments = downloader.segments.lock_sync();
        segments.set_layout_variant(1);
        segments.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 12,
                init_url: None,
                media_url,
            },
        );
    }

    assert!(
        !downloader.segment_loaded_for_demand(0, 1, "test_stale", "test_loaded"),
        "per-variant storage alone must not suppress demand when the segment is not visible in the current layout"
    );
}

#[kithara::test(tokio)]
async fn cross_variant_skip_prevents_redownload_of_committed_segment() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

    {
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("acquire media resource");
        media_res
            .write_at(0, &[0u8; 1000])
            .expect("write media data");
        media_res.commit(Some(1000)).expect("commit media");
    }

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            3,
            SegmentData {
                init_len: 0,
                media_len: 1000,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.cursor.reset_fill(3);
    let skipped = downloader.should_skip_planned_segment(1, 3, true, Some(0), false);

    assert!(
        skipped,
        "cross-variant skip must return true when old variant has the segment committed and resource-available"
    );
    assert_eq!(
        downloader.current_segment_index(),
        4,
        "cross-variant skip must advance cursor past the skipped segment"
    );
}

#[kithara::test(tokio)]
async fn cross_codec_switch_bypasses_cross_variant_skip() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::Flac), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

    {
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("acquire media resource");
        media_res
            .write_at(0, &[0u8; 1000])
            .expect("write media data");
        media_res.commit(Some(1000)).expect("commit media");
    }

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            3,
            SegmentData {
                init_len: 0,
                media_len: 1000,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.cursor.reset_fill(3);
    let skipped = downloader.should_skip_planned_segment(1, 3, true, Some(0), true);

    assert!(
        !skipped,
        "cross-codec switch must bypass cross-variant skip and re-download the segment"
    );
}

#[kithara::test(tokio)]
async fn no_cross_variant_skip_when_old_variant_is_none() {
    let cancel = CancellationToken::new();
    let playlist_state = Arc::new(PlaylistState::new(vec![
        make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
        make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
    ]));
    let variants = parsed_variants(2);
    let fetch = test_fetch_manager(cancel.clone());
    let config = HlsConfig {
        cancel: Some(cancel),
        ..HlsConfig::default()
    };

    let (mut downloader, _source) = build_pair(
        fetch,
        &variants,
        &config,
        Arc::clone(&playlist_state),
        EventBus::new(16),
    );

    let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

    {
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("acquire media resource");
        media_res
            .write_at(0, &[0u8; 1000])
            .expect("write media data");
        media_res.commit(Some(1000)).expect("commit media");
    }

    {
        let mut segments = downloader.segments.lock_sync();
        segments.commit_segment(
            0,
            3,
            SegmentData {
                init_len: 0,
                media_len: 1000,
                init_url: None,
                media_url,
            },
        );
    }

    downloader.cursor.reset_fill(3);
    let skipped = downloader.should_skip_planned_segment(1, 3, false, None, false);

    assert!(
        !skipped,
        "should_skip_planned_segment must not cross-variant skip when old_variant is None"
    );
}
