use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_hls::variant::{
    HlsVariant, InitEntry, PlanCtx, PlannedFetch, PlannedKind, SegmentEntry, SegmentState,
};
use kithara_platform::time::Duration;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{kithara, probe_capture};
use tokio_util::sync::CancellationToken;
use url::Url;

fn build_ctx(prefetch_budget: usize) -> PlanCtx {
    let cancel = CancellationToken::new();
    let passthrough: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let asset_store = Arc::new(
        AssetStoreBuilder::new()
            .ephemeral(true)
            .cancel(cancel.clone())
            .process_fn(passthrough)
            .build(),
    );
    let downloader = Arc::new(Downloader::new(
        DownloaderConfig::default().with_cancel(cancel.child_token()),
    ));
    PlanCtx {
        master_cancel: cancel,
        asset_store,
        downloader,
        prefetch_budget,
    }
}

fn make_init(size: u64) -> InitEntry {
    let url: Url = "https://example.com/init.mp4".parse().expect("url");
    let asset_id = ResourceKey::from_url(&url);
    InitEntry {
        url,
        asset_id,
        size,
        state: SegmentState::Missing,
    }
}

fn make_seg(idx: u32, byte_offset: u64, size: u64) -> SegmentEntry {
    let url: Url = format!("https://example.com/seg{idx}.m4s")
        .parse()
        .expect("url");
    let asset_id = ResourceKey::from_url(&url);
    SegmentEntry {
        url,
        asset_id,
        byte_offset,
        size,
        state: SegmentState::Missing,
        decrypt_ctx: None,
        decode_time: Duration::from_millis(u64::from(idx) * 2000),
        duration: Duration::from_secs(2),
    }
}

fn build_variant_with_inflight_queue(ctx: &PlanCtx, n_segments: u32) -> HlsVariant {
    let mut init = make_init(0);
    init.state = SegmentState::Loaded;
    let segs: Vec<SegmentEntry> = (0..n_segments)
        .map(|i| make_seg(i, u64::from(i) * 100, 100))
        .collect();
    let mut v = HlsVariant::new(0, init, segs, ctx);
    for seg in 5..8_u32 {
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(seg),
        });
    }
    v
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn rebuild_on_seek_cancels_old_fetches() {
    let recorder = probe_capture::install();
    let ctx = build_ctx(3);
    let mut v = build_variant_with_inflight_queue(&ctx, 20);
    let old_token = v.cancel.clone();
    let old_queue_len = v.queue.len();

    v.rebuild(&ctx, 10);

    assert!(old_token.is_cancelled(), "old cancel token cancelled");
    assert!(!v.cancel.is_cancelled(), "new cancel token alive");

    let evts = recorder.events_with_probe("rebuild");
    assert!(!evts.is_empty(), "rebuild probe fires");
    let evt = &evts[0];
    assert_eq!(evt.u64("variant"), Some(0));
    assert_eq!(evt.u64("from_seg"), Some(10));
    assert_eq!(evt.u64("old_queue_len"), Some(old_queue_len as u64));

    // Queue should have been refilled from seg 10 onwards, bounded by prefetch_budget=3.
    let seg_indices: Vec<u32> = v
        .queue
        .iter()
        .filter_map(|p| match p.kind {
            PlannedKind::Segment(s) => Some(s),
            PlannedKind::Init => None,
        })
        .collect();
    assert_eq!(seg_indices, vec![10, 11, 12, 13]);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn variant_flip_cancels_v_old_token() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx(3);
    let mut v_old = build_variant_with_inflight_queue(&ctx, 20);
    let mut v_new = {
        let mut init = make_init(0);
        init.state = SegmentState::Loaded;
        let segs: Vec<SegmentEntry> = (0..20)
            .map(|i| make_seg(i, u64::from(i) * 200, 200))
            .collect();
        HlsVariant::new(1, init, segs, &ctx)
    };
    let v_old_token = v_old.cancel.clone();
    let v_new_token_before = v_new.cancel.clone();

    // Variant flip flow as described in spec HlsTrack::poll_next:
    // 1. Set V_new.position to start-of-target-segment.
    // 2. Cancel V_old in-flight.
    // 3. Rebuild V_new with target seg.
    let from_seg = 7_u32;
    v_new.set_position(v_new.segments[from_seg as usize].byte_offset);
    v_old.cancel.cancel();
    v_new.rebuild(&ctx, from_seg);

    assert!(v_old_token.is_cancelled(), "V_old token cancelled by flip");
    assert!(
        v_new_token_before.is_cancelled(),
        "V_new old token replaced by rebuild"
    );
    assert!(!v_new.cancel.is_cancelled(), "V_new fresh token alive");
    assert_eq!(
        v_new.get_position(),
        v_new.segments[from_seg as usize].byte_offset
    );
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn rebuild_front_of_queue_catch_up() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx(3);
    let mut init = make_init(0);
    init.state = SegmentState::Loaded;
    let mut segs: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg(i, u64::from(i) * 100, 100))
        .collect();
    segs[10].state = SegmentState::Loaded;
    let mut v = HlsVariant::new(0, init, segs, &ctx);

    v.rebuild(&ctx, 10);

    // Front-of-queue should NOT contain seg 10 (Loaded) — first item should be next MISSING.
    let first_seg = v
        .queue
        .front()
        .and_then(|p| match p.kind {
            PlannedKind::Segment(s) => Some(s),
            PlannedKind::Init => None,
        })
        .expect("queue has segment at front");
    assert_ne!(first_seg, 10, "loaded segment must not be requeued");
    assert_eq!(
        first_seg, 11,
        "first MISSING segment from from_seg is queued"
    );
}
