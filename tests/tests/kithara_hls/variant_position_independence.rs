use std::sync::Arc;

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_hls::variant::{HlsVariant, InitEntry, PlanCtx, SegmentEntry, SegmentState};
use kithara_platform::time::Duration;
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{kithara, probe_capture};
use tokio_util::sync::CancellationToken;
use url::Url;

fn build_ctx() -> PlanCtx {
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
        prefetch_budget: 3,
    }
}

fn make_init(size: u64) -> InitEntry {
    let url: Url = "https://example.com/init.mp4".parse().expect("url");
    let asset_id = ResourceKey::from_url(&url);
    InitEntry {
        url,
        asset_id,
        size,
        state: SegmentState::Loaded,
    }
}

fn make_seg(prefix: &str, idx: u32, byte_offset: u64, size: u64) -> SegmentEntry {
    let url: Url = format!("https://example.com/{prefix}-seg{idx}.m4s")
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
async fn v_old_and_v_new_positions_independent_after_flip() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let segs_old: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg("v_old", i, u64::from(i) * 400, 400))
        .collect();
    let segs_new: Vec<SegmentEntry> = (0..20)
        .map(|i| make_seg("v_new", i, u64::from(i) * 800, 800))
        .collect();
    let v_old = HlsVariant::new(0, make_init(0), segs_old, &ctx);
    let v_new = HlsVariant::new(1, make_init(0), segs_new, &ctx);

    v_old.set_position(5000);
    v_new.set_position(v_new.segments[10].byte_offset);

    assert_eq!(v_old.get_position(), 5000, "V_old position unchanged");
    assert_eq!(
        v_new.get_position(),
        v_new.segments[10].byte_offset,
        "V_new positioned at start of seg 10"
    );

    v_new.advance(123);
    assert_eq!(
        v_old.get_position(),
        5000,
        "V_old position unaffected by V_new advance"
    );
    assert_eq!(v_new.get_position(), v_new.segments[10].byte_offset + 123);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn advance_only_changes_active_variant_position() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let v_old = HlsVariant::new(0, make_init(0), vec![make_seg("v_old", 0, 0, 100)], &ctx);
    let v_new = HlsVariant::new(1, make_init(0), vec![make_seg("v_new", 0, 0, 100)], &ctx);

    v_old.set_position(5000);
    v_new.set_position(7200);

    // Simulate Source::advance routing to active variant (V_new in this test).
    v_new.advance(500);

    assert_eq!(v_new.get_position(), 7700);
    assert_eq!(v_old.get_position(), 5000, "V_old untouched");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn position_monotonic_within_one_variant() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let v = HlsVariant::new(0, make_init(0), vec![make_seg("v", 0, 0, 100)], &ctx);
    let advances: Vec<u64> = vec![10, 25, 7, 64, 1, 100];
    let mut expected = 0_u64;
    let mut observed = Vec::with_capacity(advances.len());
    for n in advances {
        v.advance(n);
        expected += n;
        observed.push(v.get_position());
        assert_eq!(v.get_position(), expected);
    }
    let mut sorted = observed.clone();
    sorted.sort_unstable();
    assert_eq!(observed, sorted, "position values are monotonic increasing");
}
