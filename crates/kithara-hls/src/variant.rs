#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    ops::Range,
    sync::{Arc, atomic::AtomicU64},
};

use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Duration;
use kithara_stream::{
    SegmentDescriptor,
    dl::{Downloader, FetchCmd},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

pub type AssetId = ResourceKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    Missing,
    Downloading,
    Loaded,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SegmentEntry {
    pub url: Url,
    pub asset_id: AssetId,
    /// In THIS variant's byte space; starts at `init.size` for seg[0].
    pub byte_offset: u64,
    pub size: u64,
    pub state: SegmentState,
    pub decrypt_ctx: Option<DecryptContext>,
    pub decode_time: Duration,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InitEntry {
    pub url: Url,
    pub asset_id: AssetId,
    pub size: u64,
    pub state: SegmentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedKind {
    Init,
    Segment(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlannedFetch {
    pub variant: usize,
    pub kind: PlannedKind,
}

#[non_exhaustive]
pub struct PlanCtx {
    pub master_cancel: CancellationToken,
    pub asset_store: Arc<AssetStore<DecryptContext>>,
    pub downloader: Arc<Downloader>,
    pub prefetch_budget: usize,
}

#[non_exhaustive]
pub struct HlsVariant {
    pub variant: usize,
    pub init: InitEntry,
    pub segments: Vec<SegmentEntry>,
    pub queue: VecDeque<PlannedFetch>,
    pub cancel: CancellationToken,
    pub position: Arc<AtomicU64>,
}

impl HlsVariant {
    #[must_use]
    pub fn new(
        variant: usize,
        init: InitEntry,
        segments: Vec<SegmentEntry>,
        ctx: &PlanCtx,
    ) -> Self {
        Self {
            variant,
            init,
            segments,
            queue: VecDeque::new(),
            cancel: ctx.master_cancel.child_token(),
            position: Arc::new(AtomicU64::new(0)),
        }
    }

    #[kithara::probe]
    pub fn get_position(&self) -> u64 {
        unimplemented!("Plan 03 — HlsVariant::get_position")
    }

    #[kithara::probe]
    pub fn advance(&self, _n: u64) {
        unimplemented!("Plan 03 — HlsVariant::advance")
    }

    #[kithara::probe]
    pub fn set_position(&self, _pos: u64) {
        unimplemented!("Plan 03 — HlsVariant::set_position")
    }

    #[kithara::probe]
    pub fn find_at_offset(&self, _byte_offset: u64) -> Option<(u32, &SegmentEntry)> {
        unimplemented!("Plan 03 — HlsVariant::find_at_offset")
    }

    #[kithara::probe]
    pub fn total_bytes(&self) -> u64 {
        unimplemented!("Plan 03 — HlsVariant::total_bytes")
    }

    #[must_use]
    pub fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    #[kithara::probe]
    pub fn init_byte_range(&self) -> Option<Range<u64>> {
        unimplemented!("Plan 03 — HlsVariant::init_byte_range")
    }

    #[kithara::probe]
    pub fn descriptor_at_time(&self, _t: Duration) -> Option<SegmentDescriptor> {
        unimplemented!("Plan 03 — HlsVariant::descriptor_at_time")
    }

    #[kithara::probe]
    pub fn descriptor_after_byte(&self, _byte: u64) -> Option<SegmentDescriptor> {
        unimplemented!("Plan 03 — HlsVariant::descriptor_after_byte")
    }

    #[kithara::probe]
    pub fn rebuild(&mut self, _ctx: &PlanCtx, _from_seg: u32) {
        unimplemented!("Plan 03 — HlsVariant::rebuild")
    }

    #[kithara::probe]
    pub fn on_reader_advance(&mut self, _ctx: &PlanCtx, _seg_at_reader: u32) {
        unimplemented!("Plan 03 — HlsVariant::on_reader_advance")
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `asset_id` doesn't belong to this variant.
    #[kithara::probe]
    pub fn on_evict(&mut self, _asset_id: &AssetId) -> Option<i32> {
        unimplemented!("Plan 03 — HlsVariant::on_evict")
    }

    #[kithara::probe]
    pub fn dispatch(&mut self, _ctx: &PlanCtx, _budget: usize) -> Vec<FetchCmd> {
        unimplemented!("Plan 03 — HlsVariant::dispatch")
    }
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_stream::dl::{Downloader, DownloaderConfig};

    use super::*;

    /// Plan 00 marker test: types compile and can be instantiated.
    /// Plan 03 replaces this with real behavior tests.
    #[test]
    fn skeleton_types_instantiate() {
        let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
        let asset_id: AssetId = ResourceKey::from_url(&url);
        let _seg = SegmentEntry {
            url: url.clone(),
            asset_id: asset_id.clone(),
            byte_offset: 0,
            size: 100,
            state: SegmentState::Missing,
            decrypt_ctx: None,
            decode_time: Duration::ZERO,
            duration: Duration::from_secs(2),
        };
        let init = InitEntry {
            url: url.clone(),
            asset_id,
            size: 200,
            state: SegmentState::Missing,
        };
        let _pf = PlannedFetch {
            variant: 0,
            kind: PlannedKind::Init,
        };
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
        let ctx = PlanCtx {
            master_cancel: cancel,
            asset_store: backend,
            downloader,
            prefetch_budget: 3,
        };
        let variant = HlsVariant::new(0, init, Vec::new(), &ctx);
        assert_eq!(variant.num_segments(), 0);
    }
}
