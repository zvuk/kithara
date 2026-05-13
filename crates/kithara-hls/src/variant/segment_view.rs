#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_platform::time::Duration;
use kithara_stream::{SegmentDescriptor, SegmentLayout};
use kithara_test_utils::kithara;

use super::HlsVariant;

/// Lock-free `SegmentLayout` impl. Active variant lookup via `Acquire` load
/// on `AtomicUsize`; variant shape (`Arc<Vec<HlsVariant>>`) is immutable.
#[non_exhaustive]
pub(crate) struct HlsSegmentView {
    variants: Arc<Vec<HlsVariant>>,
    active_variant: Arc<AtomicUsize>,
}

impl HlsSegmentView {
    #[must_use]
    pub(crate) fn new(variants: Arc<Vec<HlsVariant>>, active_variant: Arc<AtomicUsize>) -> Self {
        Self {
            variants,
            active_variant,
        }
    }

    fn active(&self) -> Option<&HlsVariant> {
        let idx = self.active_variant.load(Ordering::Acquire);
        self.variants.get(idx)
    }
}

impl SegmentLayout for HlsSegmentView {
    #[kithara::probe]
    fn init_segment_range(&self) -> Option<Range<u64>> {
        self.active()?.init_byte_range()
    }

    #[kithara::probe(byte)]
    fn segment_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_after_byte(byte)
    }

    #[kithara::probe]
    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.active()?.descriptor_at_time(t)
    }

    fn segment_count(&self) -> Option<u32> {
        Some(self.active()?.num_segments())
    }

    fn len(&self) -> Option<u64> {
        Some(self.active()?.total_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicBool, Ordering as TestOrdering},
        },
        thread,
    };

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
    use kithara_drm::DecryptContext;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        super::{HlsVariant, InitEntry, PlanCtx, SegmentEntry, SegmentState},
        *,
    };

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
        PlanCtx {
            master_cancel: cancel,
            asset_store: backend,
            prefetch_budget: 3,
        }
    }

    fn make_init(size: u64, tag: &str) -> InitEntry {
        let url: Url = format!("https://example.com/{tag}/init.mp4")
            .parse()
            .expect("valid url");
        let resource_id = ResourceKey::from_url(&url);
        InitEntry {
            url,
            resource_id,
            size,
            state: SegmentState::Missing.into(),
        }
    }

    fn make_seg(tag: &str, idx: u32, byte_offset: u64, size: u64) -> SegmentEntry {
        let url: Url = format!("https://example.com/{tag}/seg{idx}.m4s")
            .parse()
            .expect("valid url");
        let resource_id = ResourceKey::from_url(&url);
        SegmentEntry {
            url,
            resource_id,
            byte_offset,
            size,
            state: SegmentState::Missing.into(),
            decrypt_ctx: None,
            decode_time: Duration::from_millis(u64::from(idx) * 2000),
            duration: Duration::from_secs(2),
        }
    }

    fn variant_low(ctx: &PlanCtx) -> HlsVariant {
        HlsVariant::from_parts(
            0,
            make_init(100, "lo"),
            vec![
                make_seg("lo", 0, 100, 200),
                make_seg("lo", 1, 300, 200),
                make_seg("lo", 2, 500, 200),
            ],
            ctx,
        )
    }

    fn variant_high(ctx: &PlanCtx) -> HlsVariant {
        HlsVariant::from_parts(
            1,
            make_init(300, "hi"),
            vec![
                make_seg("hi", 0, 300, 400),
                make_seg("hi", 1, 700, 400),
                make_seg("hi", 2, 1100, 400),
            ],
            ctx,
        )
    }

    #[kithara::test]
    fn init_segment_range_follows_active_variant() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
        let active = Arc::new(AtomicUsize::new(0));
        let view = HlsSegmentView::new(Arc::clone(&variants), Arc::clone(&active));
        assert_eq!(view.init_segment_range(), Some(0..100));
        active.store(1, Ordering::Release);
        assert_eq!(view.init_segment_range(), Some(0..300));
    }

    #[kithara::test]
    fn segment_at_time_routes_through_active() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
        let active = Arc::new(AtomicUsize::new(0));
        let view = HlsSegmentView::new(Arc::clone(&variants), Arc::clone(&active));
        let desc = view
            .segment_at_time(Duration::from_secs(2))
            .expect("seg present");
        assert_eq!(desc.segment_index, 1);
        assert_eq!(desc.variant_index, 0);
        active.store(1, Ordering::Release);
        let desc = view
            .segment_at_time(Duration::from_secs(2))
            .expect("seg present");
        assert_eq!(desc.variant_index, 1);
    }

    #[kithara::test]
    fn segment_after_byte_routes_through_active() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
        let active = Arc::new(AtomicUsize::new(0));
        let view = HlsSegmentView::new(Arc::clone(&variants), Arc::clone(&active));
        let desc = view.segment_after_byte(150).expect("seg present");
        assert_eq!(desc.variant_index, 0);
        active.store(1, Ordering::Release);
        let desc = view.segment_after_byte(150).expect("seg present");
        assert_eq!(desc.variant_index, 1);
        assert_eq!(desc.segment_index, 0);
    }

    #[kithara::test]
    fn segment_count_and_len_track_active() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
        let active = Arc::new(AtomicUsize::new(0));
        let view = HlsSegmentView::new(Arc::clone(&variants), Arc::clone(&active));
        assert_eq!(view.segment_count(), Some(3));
        assert_eq!(view.len(), Some(700));
        active.store(1, Ordering::Release);
        assert_eq!(view.segment_count(), Some(3));
        assert_eq!(view.len(), Some(1500));
    }

    #[kithara::test]
    fn empty_variant_returns_none() {
        let view = HlsSegmentView::new(Arc::new(Vec::new()), Arc::new(AtomicUsize::new(0)));
        assert!(view.init_segment_range().is_none());
        assert!(view.segment_after_byte(0).is_none());
        assert!(view.segment_at_time(Duration::from_secs(0)).is_none());
        assert!(view.segment_count().is_none());
        assert!(view.len().is_none());
    }

    #[kithara::test]
    fn out_of_bounds_active_idx_returns_none() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx)]);
        let view = HlsSegmentView::new(Arc::clone(&variants), Arc::new(AtomicUsize::new(7)));
        assert!(view.init_segment_range().is_none());
        assert!(view.segment_count().is_none());
        assert!(view.len().is_none());
    }

    #[kithara::test]
    fn concurrent_reads_consistent_with_flips() {
        let ctx = test_ctx();
        let variants = Arc::new(vec![variant_low(&ctx), variant_high(&ctx)]);
        let active = Arc::new(AtomicUsize::new(0));
        let view = Arc::new(HlsSegmentView::new(
            Arc::clone(&variants),
            Arc::clone(&active),
        ));

        let stop = Arc::new(AtomicBool::new(false));
        let reader_count = 4_usize;
        let iter_count = 4_000_usize;
        let barrier = Arc::new(Barrier::new(reader_count + 1));

        let mut readers = Vec::with_capacity(reader_count);
        for _ in 0..reader_count {
            let view = Arc::clone(&view);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            readers.push(thread::spawn(move || {
                barrier.wait();
                while !stop.load(TestOrdering::Acquire) {
                    if let Some(range) = view.init_segment_range() {
                        assert!(range.start == 0);
                        assert!(matches!(range.end, 100 | 300));
                    }
                    if let Some(total) = view.len() {
                        assert!(matches!(total, 700 | 1500));
                    }
                    if let Some(desc) = view.segment_at_time(Duration::from_secs(2)) {
                        assert!(matches!(desc.variant_index, 0 | 1));
                    }
                    if let Some(count) = view.segment_count() {
                        assert_eq!(count, 3, "both variants have 3 segments — no torn read");
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
                writer_view
                    .active_variant
                    .store(i & 1, TestOrdering::Release);
            }
            writer_stop.store(true, TestOrdering::Release);
        });

        writer.join().expect("writer joined");
        for r in readers {
            r.join().expect("reader joined");
        }
    }
}
