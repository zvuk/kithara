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
async fn offset_below_init_size_resolves_to_init() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    // init.size=200; segments[0] starts at byte_offset=200.
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![make_seg(0, 200, 400), make_seg(1, 600, 400)],
        &ctx,
    );
    for offset in [0_u64, 150, 199] {
        assert!(
            v.find_at_offset(offset).is_none(),
            "offset {offset} below init.size must NOT resolve to a segment (lives in init space)"
        );
    }
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn offset_at_init_size_resolves_to_segment_0() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![make_seg(0, 200, 400), make_seg(1, 600, 400)],
        &ctx,
    );
    let (idx, entry) = v.find_at_offset(200).expect("hit");
    assert_eq!(idx, 0);
    assert_eq!(entry.byte_offset, 200);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn offset_mid_segment_binary_search() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![
            make_seg(0, 200, 400),
            make_seg(1, 600, 400),
            make_seg(2, 1000, 400),
            make_seg(3, 1400, 400),
        ],
        &ctx,
    );
    let (idx, _) = v.find_at_offset(750).expect("hit");
    assert_eq!(idx, 1, "offset 750 lives inside segments[1] (600..1000)");
    let (idx, _) = v.find_at_offset(1399).expect("hit");
    assert_eq!(idx, 2);
    let (idx, _) = v.find_at_offset(1400).expect("hit");
    assert_eq!(idx, 3);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn total_bytes_includes_init_plus_segments() {
    let _recorder = probe_capture::install();
    let ctx = build_ctx();
    let v = HlsVariant::new(
        0,
        make_init(200),
        vec![
            make_seg(0, 200, 400),
            make_seg(1, 600, 400),
            make_seg(2, 1000, 400),
            make_seg(3, 1400, 400),
        ],
        &ctx,
    );
    assert_eq!(v.total_bytes(), 1800, "200 init + 4 * 400 segments");
}
