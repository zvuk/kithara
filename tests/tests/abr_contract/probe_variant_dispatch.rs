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

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_respects_budget() {
    let recorder = probe_capture::install();
    let ctx = build_ctx(5);
    let mut init = make_init(0);
    init.state = SegmentState::Loaded;
    let segs: Vec<SegmentEntry> = (0..10)
        .map(|i| make_seg(i, u64::from(i) * 100, 100))
        .collect();
    let mut v = HlsVariant::new(0, init, segs, &ctx);
    for seg in 5..10_u32 {
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(seg),
        });
    }
    let queue_len_before = v.queue.len();

    let cmds = v.dispatch(&ctx, 3);

    assert_eq!(cmds.len(), 3, "exactly budget=3 cmds emitted");
    assert_eq!(
        v.queue.len(),
        queue_len_before - 3,
        "queue shrinks by budget"
    );

    let evts = recorder.events_with_probe("dispatch");
    assert!(
        !evts.is_empty(),
        "HlsVariant::dispatch probe must fire at least once"
    );
    let evt = &evts[0];
    assert_eq!(evt.u64("variant"), Some(0));
    assert_eq!(evt.u64("budget"), Some(3));
    assert_eq!(evt.u64("queue_len"), Some(queue_len_before as u64));
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_emits_init_first() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx(5);
    let init = make_init(200);
    let segs = vec![make_seg(0, 200, 400), make_seg(1, 600, 400)];
    let mut v = HlsVariant::new(0, init, segs, &ctx);
    for seg in 0..2_u32 {
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(seg),
        });
    }
    let init_url = v.init.url.clone();
    let seg0_url = v.segments[0].url.clone();
    let seg1_url = v.segments[1].url.clone();

    let cmds = v.dispatch(&ctx, 10);

    assert_eq!(cmds.len(), 3, "init + 2 segments");
    assert_eq!(cmds[0].url, init_url, "first cmd is init fetch");
    assert_eq!(cmds[1].url, seg0_url, "second cmd is seg0");
    assert_eq!(cmds[2].url, seg1_url, "third cmd is seg1");
    assert_eq!(v.init.state, SegmentState::Downloading);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_skips_non_missing_segments() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx(5);
    let mut init = make_init(0);
    init.state = SegmentState::Loaded;
    let segs = vec![
        make_seg(0, 0, 100),
        make_seg(1, 100, 100),
        make_seg(2, 200, 100),
    ];
    let mut v = HlsVariant::new(0, init, segs, &ctx);
    v.segments[1].state = SegmentState::Loaded;
    for seg in 0..3_u32 {
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(seg),
        });
    }

    let cmds = v.dispatch(&ctx, 10);

    assert_eq!(cmds.len(), 2, "loaded segment is skipped");
    assert_eq!(v.segments[1].state, SegmentState::Loaded);
    assert_eq!(v.segments[0].state, SegmentState::Downloading);
    assert_eq!(v.segments[2].state, SegmentState::Downloading);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_attaches_variant_cancel_token() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx(5);
    let mut init = make_init(0);
    init.state = SegmentState::Loaded;
    let segs = vec![make_seg(0, 0, 100), make_seg(1, 100, 100)];
    let mut v = HlsVariant::new(0, init, segs, &ctx);
    let variant_cancel = v.cancel.clone();
    for seg in 0..2_u32 {
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(seg),
        });
    }

    let cmds = v.dispatch(&ctx, 10);

    for cmd in &cmds {
        let token = cmd.cancel.as_ref().expect("cmd carries cancel");
        assert!(!token.is_cancelled(), "fresh cmd token not yet cancelled");
    }
    assert!(!variant_cancel.is_cancelled());
    variant_cancel.cancel();
    for cmd in &cmds {
        let token = cmd.cancel.as_ref().expect("cmd carries cancel");
        assert!(
            token.is_cancelled(),
            "cmd token shares cancellation with variant.cancel"
        );
    }
}
