use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_hls::{
    segment_view::HlsSegmentView,
    variant::{HlsVariant, InitEntry, PlanCtx, SegmentEntry, SegmentState},
};
use kithara_platform::time::Duration;
use kithara_stream::{
    SegmentLayout,
    dl::{Downloader, DownloaderConfig},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

fn test_ctx() -> PlanCtx {
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
        prefetch_budget: 3,
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

fn variant_low(ctx: &PlanCtx) -> HlsVariant {
    HlsVariant::new(
        0,
        init_entry("lo", 100),
        vec![
            seg_entry("lo", 0, 100, 200),
            seg_entry("lo", 1, 300, 200),
            seg_entry("lo", 2, 500, 200),
        ],
        ctx,
    )
}

fn variant_high(ctx: &PlanCtx) -> HlsVariant {
    HlsVariant::new(
        1,
        init_entry("hi", 300),
        vec![
            seg_entry("hi", 0, 300, 400),
            seg_entry("hi", 1, 700, 400),
            seg_entry("hi", 2, 1100, 400),
        ],
        ctx,
    )
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn lockfree_concurrent_query_and_flip_no_torn_read() {
    let ctx = test_ctx();
    let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
    let active = Arc::new(AtomicUsize::new(0));
    let view = Arc::new(HlsSegmentView::new(
        Arc::clone(&variants),
        Arc::clone(&active),
    ));

    let stop = Arc::new(AtomicBool::new(false));
    let reader_count = 4_usize;
    let iter_count = 1_000_usize;
    let barrier = Arc::new(Barrier::new(reader_count + 1));

    let mut readers = Vec::with_capacity(reader_count);
    for _ in 0..reader_count {
        let view = Arc::clone(&view);
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            barrier.wait();
            while !stop.load(Ordering::Acquire) {
                if let Some(range) = view.init_segment_range() {
                    assert_eq!(range.start, 0);
                    assert!(
                        matches!(range.end, 100 | 300),
                        "torn init range observed: {range:?}"
                    );
                }
                if let Some(total) = view.len() {
                    assert!(matches!(total, 700 | 1500), "torn total observed: {total}");
                }
                if let Some(desc) = view.segment_at_time(Duration::from_secs(2)) {
                    assert!(matches!(desc.variant_index, 0 | 1));
                }
                if let Some(count) = view.segment_count() {
                    assert_eq!(count, 3);
                }
            }
        }));
    }

    let writer_view = Arc::clone(&view);
    let writer_stop = Arc::clone(&stop);
    let writer_barrier = Arc::clone(&barrier);
    let writer = thread::spawn(move || {
        writer_barrier.wait();
        for i in 0..iter_count {
            writer_view.active_variant.store(i & 1, Ordering::Release);
        }
        writer_stop.store(true, Ordering::Release);
    });

    writer.join().expect("writer joined");
    for r in readers {
        r.join().expect("reader joined");
    }
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn init_segment_range_follows_active_variant() {
    let ctx = test_ctx();
    let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
    let active = Arc::new(AtomicUsize::new(0));
    let view = HlsSegmentView::new(Arc::clone(&variants), Arc::clone(&active));

    assert_eq!(view.init_segment_range(), Some(0..100));
    assert_eq!(view.len(), Some(700));

    active.store(1, Ordering::Release);
    assert_eq!(view.init_segment_range(), Some(0..300));
    assert_eq!(view.len(), Some(1500));
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn empty_variant_returns_none() {
    let view = HlsSegmentView::new(Arc::new(Vec::new()), Arc::new(AtomicUsize::new(0)));
    assert!(view.init_segment_range().is_none());
    assert!(view.segment_after_byte(0).is_none());
    assert!(view.segment_at_time(Duration::from_secs(0)).is_none());
    assert!(view.segment_count().is_none());
    assert!(view.len().is_none());
}
