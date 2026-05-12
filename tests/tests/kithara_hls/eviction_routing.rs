use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, OnInvalidatedFn, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_hls::variant::{
    HlsVariant, InitEntry, PlanCtx, PlannedFetch, PlannedKind, SegmentEntry, SegmentState,
};
use kithara_platform::time::Duration;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::kithara;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

fn test_ctx(budget: usize) -> PlanCtx {
    let cancel = CancellationToken::new();
    let passthrough: ProcessChunkFn<DecryptContext> =
        Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });
    let backend = Arc::new(
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
        asset_store: backend,
        downloader,
        prefetch_budget: budget,
    }
}

fn init_entry(tag: &str, size: u64) -> InitEntry {
    let url: Url = format!("https://example.com/{tag}/init.mp4")
        .parse()
        .expect("valid url");
    let asset_id = ResourceKey::from_url(&url);
    InitEntry {
        url,
        asset_id,
        size,
        state: SegmentState::Missing,
    }
}

fn seg_entry(tag: &str, idx: u32, byte_offset: u64, size: u64) -> SegmentEntry {
    let url: Url = format!("https://example.com/{tag}/seg{idx}.m4s")
        .parse()
        .expect("valid url");
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

fn track_callback(tx: mpsc::UnboundedSender<ResourceKey>) -> OnInvalidatedFn {
    Arc::new(move |key: &ResourceKey| {
        let _ = tx.send(key.clone());
    })
}

/// Drain `rx` and apply the post-Plan-06 routing rule: each evicted
/// `ResourceKey` is broadcast to every variant via `on_evict`; the
/// active variant additionally re-enqueues at the front when the
/// evicted segment lies inside the visibility window
/// `[reader_seg, reader_seg + prefetch_budget]`.
fn drain_and_route(
    rx: &mut mpsc::UnboundedReceiver<ResourceKey>,
    variants: &mut [HlsVariant],
    active: usize,
    reader_seg: u32,
    budget: u32,
) {
    while let Ok(key) = rx.try_recv() {
        for v in variants.iter_mut() {
            let Some(evicted) = v.on_evict(&key) else {
                continue;
            };
            if v.variant != active || evicted < 0 {
                continue;
            }
            let seg = u32::try_from(evicted).expect("evicted seg fits u32");
            let upper = reader_seg.saturating_add(budget);
            if seg >= reader_seg && seg <= upper {
                v.queue.push_front(PlannedFetch {
                    variant: v.variant,
                    kind: PlannedKind::Segment(seg),
                });
            }
        }
    }
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn eviction_routed_only_to_owning_track() {
    let (tx_a, mut rx_a) = mpsc::unbounded_channel::<ResourceKey>();
    let (tx_b, mut rx_b) = mpsc::unbounded_channel::<ResourceKey>();

    let cb_a = track_callback(tx_a);
    let cb_b = track_callback(tx_b);

    let key: Url = "https://example.com/track_a/seg42.m4s"
        .parse()
        .expect("valid url");
    let rk = ResourceKey::from_url(&key);

    cb_a(&rk);

    assert_eq!(
        rx_a.try_recv().ok(),
        Some(rk.clone()),
        "owning track's mpsc must receive the eviction event"
    );
    assert!(
        matches!(rx_b.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "foreign track's mpsc must stay empty (per-track AssetStore isolation)"
    );
    assert!(
        matches!(rx_a.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "owning track's mpsc has no further pending events"
    );
    // Keep cb_b alive so the type is exercised even when no key flows through.
    drop(cb_b);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn on_evict_checks_init_then_segments() {
    let ctx = test_ctx(3);
    let mut v = HlsVariant::new(
        0,
        init_entry("v0", 200),
        vec![
            seg_entry("v0", 0, 200, 200),
            seg_entry("v0", 1, 400, 200),
            seg_entry("v0", 2, 600, 200),
        ],
        &ctx,
    );
    v.init.state = SegmentState::Loaded;
    v.segments[1].state = SegmentState::Loaded;

    let init_key = v.init.asset_id.clone();
    let seg1_key = v.segments[1].asset_id.clone();

    assert_eq!(v.on_evict(&init_key), Some(-1));
    assert_eq!(v.init.state, SegmentState::Missing);
    assert_eq!(
        v.segments[1].state,
        SegmentState::Loaded,
        "init eviction must not touch segment states"
    );

    assert_eq!(v.on_evict(&seg1_key), Some(1));
    assert_eq!(v.segments[1].state, SegmentState::Missing);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn foreign_asset_id_ignored() {
    let ctx = test_ctx(3);
    let mut v = HlsVariant::new(
        0,
        init_entry("v0", 100),
        vec![seg_entry("v0", 0, 100, 200), seg_entry("v0", 1, 300, 200)],
        &ctx,
    );
    v.init.state = SegmentState::Loaded;
    v.segments[0].state = SegmentState::Loaded;
    v.segments[1].state = SegmentState::Loaded;

    let foreign: Url = "https://other.example.com/foreign.m4s"
        .parse()
        .expect("valid url");
    let foreign_key = ResourceKey::from_url(&foreign);

    assert_eq!(v.on_evict(&foreign_key), None);
    assert_eq!(v.init.state, SegmentState::Loaded);
    assert_eq!(v.segments[0].state, SegmentState::Loaded);
    assert_eq!(v.segments[1].state, SegmentState::Loaded);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn active_variant_eviction_in_visibility_window_re_enqueues_front() {
    let ctx = test_ctx(3);
    let mut v0 = HlsVariant::new(
        0,
        init_entry("v0", 0),
        (0..15)
            .map(|i| seg_entry("v0", i, u64::from(i) * 100, 100))
            .collect(),
        &ctx,
    );
    for seg in &mut v0.segments {
        seg.state = SegmentState::Loaded;
    }
    let mut v1 = HlsVariant::new(
        1,
        init_entry("v1", 0),
        (0..15)
            .map(|i| seg_entry("v1", i, u64::from(i) * 100, 100))
            .collect(),
        &ctx,
    );
    for seg in &mut v1.segments {
        seg.state = SegmentState::Loaded;
    }

    let active_evicted_in_window = v0.segments[11].asset_id.clone();
    let inactive_evicted = v1.segments[7].asset_id.clone();

    let (tx, mut rx) = mpsc::unbounded_channel::<ResourceKey>();
    tx.send(active_evicted_in_window.clone())
        .expect("send active eviction");
    tx.send(inactive_evicted.clone())
        .expect("send inactive eviction");
    drop(tx);

    let mut variants = vec![v0, v1];
    let active = 0_usize;
    let reader_seg = 10_u32;
    let budget = ctx.prefetch_budget as u32;

    drain_and_route(&mut rx, &mut variants, active, reader_seg, budget);

    let v0 = &variants[0];
    let v1 = &variants[1];

    assert_eq!(
        v0.segments[11].state,
        SegmentState::Missing,
        "active variant's evicted segment must be marked missing"
    );
    assert_eq!(
        v0.queue.front().copied(),
        Some(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(11),
        }),
        "active+in-window eviction must re-enqueue at FRONT for catch-up fetch"
    );

    assert_eq!(
        v1.segments[7].state,
        SegmentState::Missing,
        "inactive variant's on_evict still flips state to missing"
    );
    assert!(
        v1.queue.is_empty(),
        "inactive variant must NOT re-enqueue (only active variant catches up)"
    );

    let out_of_window_seg = variants[0]
        .segments
        .last()
        .expect("segs present")
        .asset_id
        .clone();
    let (tx2, mut rx2) = mpsc::unbounded_channel::<ResourceKey>();
    tx2.send(out_of_window_seg).expect("send oow eviction");
    drop(tx2);
    drain_and_route(&mut rx2, &mut variants, active, reader_seg, budget);
    let v0 = &variants[0];
    let last_idx = v0.segments.len() - 1;
    assert_eq!(
        v0.segments[last_idx].state,
        SegmentState::Missing,
        "out-of-window eviction still marks state missing"
    );
    let last_idx_u32 = u32::try_from(last_idx).expect("seg index fits u32");
    assert!(
        !v0.queue
            .iter()
            .any(|p| matches!(p.kind, PlannedKind::Segment(s) if s == last_idx_u32)),
        "out-of-window eviction must NOT re-enqueue (lies beyond reader+budget)"
    );
}
