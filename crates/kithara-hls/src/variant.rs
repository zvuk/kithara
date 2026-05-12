#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::NetError;
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
use kithara_stream::{
    SegmentDescriptor,
    dl::{Downloader, FetchCmd, OnCompleteFn, WriterFn},
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
pub struct PlannedFetch {
    pub variant: usize,
    pub kind: PlannedKind,
}

pub struct PlanCtx {
    pub master_cancel: CancellationToken,
    pub asset_store: Arc<AssetStore<DecryptContext>>,
    pub downloader: Arc<Downloader>,
    pub prefetch_budget: usize,
}

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

    #[kithara::probe(variant = self.variant as u64, pos = self.position.load(Ordering::Acquire))]
    pub fn get_position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = find_at_offset_probe_seg(self, byte_offset)
    )]
    pub fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, &SegmentEntry)> {
        let idx = self.binary_search_byte(byte_offset)?;
        let entry = &self.segments[idx];
        let idx_u32 = u32::try_from(idx).ok()?;
        Some((idx_u32, entry))
    }

    #[kithara::probe(variant = self.variant as u64, total = self.total_bytes_inner())]
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes_inner()
    }

    #[must_use]
    pub fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    #[kithara::probe(variant = self.variant as u64, size = self.init.size)]
    pub fn init_byte_range(&self) -> Option<Range<u64>> {
        if self.init.size > 0 {
            Some(0..self.init.size)
        } else {
            None
        }
    }

    #[kithara::probe(variant = self.variant as u64)]
    pub fn descriptor_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        self.descriptor(idx)
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub fn descriptor_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let mut idx = bisect_left_byte_offset(&self.segments, byte);
        if idx >= self.segments.len() {
            return None;
        }
        if self.segments[idx].byte_offset < byte {
            idx += 1;
        }
        if idx >= self.segments.len() {
            return None;
        }
        self.descriptor(idx)
    }

    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.len() as u64
    )]
    pub fn rebuild(&mut self, ctx: &PlanCtx, from_seg: u32) {
        self.cancel.cancel();
        self.cancel = ctx.master_cancel.child_token();
        self.queue.clear();
        self.fill_queue(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, seg_at_reader)]
    pub fn on_reader_advance(&mut self, ctx: &PlanCtx, seg_at_reader: u32) {
        if matches!(self.state_of(seg_at_reader), SegmentState::Missing)
            && !self.queue_contains_seg(seg_at_reader)
        {
            self.queue.push_front(PlannedFetch {
                variant: self.variant,
                kind: PlannedKind::Segment(seg_at_reader),
            });
        }
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = seg_at_reader.saturating_add(budget).min(segs_len_u32 - 1);
        let last_planned = self.last_planned_seg().unwrap_or(seg_at_reader);
        let start = last_planned.saturating_add(1);
        for seg in start..=end {
            if matches!(self.state_of(seg), SegmentState::Missing) && !self.queue_contains_seg(seg)
            {
                self.queue.push_back(PlannedFetch {
                    variant: self.variant,
                    kind: PlannedKind::Segment(seg),
                });
            }
        }
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `asset_id` doesn't belong to this variant.
    #[kithara::probe(variant = self.variant as u64)]
    pub fn on_evict(&mut self, asset_id: &AssetId) -> Option<i32> {
        if &self.init.asset_id == asset_id {
            self.init.state = SegmentState::Missing;
            return Some(-1);
        }
        for (seg_idx, entry) in self.segments.iter_mut().enumerate() {
            if &entry.asset_id == asset_id {
                entry.state = SegmentState::Missing;
                return i32::try_from(seg_idx).ok();
            }
        }
        None
    }

    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.queue.len() as u64
    )]
    pub fn dispatch(&mut self, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        if matches!(self.init.state, SegmentState::Missing) && remaining > 0 {
            self.init.state = SegmentState::Downloading;
            out.push(self.build_init_cmd(ctx));
            remaining -= 1;
        }
        while remaining > 0 {
            let Some(planned) = self.queue.pop_front() else {
                break;
            };
            let PlannedKind::Segment(seg_idx) = planned.kind else {
                continue;
            };
            let Some(entry_state) = self.segments.get(seg_idx as usize).map(|e| e.state) else {
                continue;
            };
            if !matches!(entry_state, SegmentState::Missing) {
                continue;
            }
            let cmd = self.build_seg_cmd(ctx, seg_idx);
            self.segments[seg_idx as usize].state = SegmentState::Downloading;
            out.push(cmd);
            remaining -= 1;
        }
        out
    }

    fn binary_search_byte(&self, byte_offset: u64) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        // Find segment where byte_offset ∈ [segment.byte_offset, segment.byte_offset + segment.size)
        let mut lo = 0_usize;
        let mut hi = self.segments.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = &self.segments[mid];
            if byte_offset < entry.byte_offset {
                hi = mid;
            } else if byte_offset >= entry.byte_offset + entry.size {
                lo = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    fn total_bytes_inner(&self) -> u64 {
        self.segments
            .last()
            .map_or(self.init.size, |last| last.byte_offset + last.size)
    }

    fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
        let entry = self.segments.get(idx)?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        Some(SegmentDescriptor::new(
            entry.byte_offset..entry.byte_offset + entry.size,
            entry.decode_time,
            entry.duration,
            seg_idx_u32,
            self.variant,
        ))
    }

    fn fill_queue(&mut self, ctx: &PlanCtx, from_seg: u32) {
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = from_seg.saturating_add(budget).min(segs_len_u32 - 1);
        for seg in from_seg..=end {
            if matches!(self.state_of(seg), SegmentState::Missing) {
                self.queue.push_back(PlannedFetch {
                    variant: self.variant,
                    kind: PlannedKind::Segment(seg),
                });
            }
        }
    }

    fn state_of(&self, seg_idx: u32) -> SegmentState {
        self.segments
            .get(seg_idx as usize)
            .map_or(SegmentState::Missing, |e| e.state)
    }

    fn queue_contains_seg(&self, seg_idx: u32) -> bool {
        self.queue
            .iter()
            .any(|p| matches!(p.kind, PlannedKind::Segment(s) if s == seg_idx))
    }

    fn last_planned_seg(&self) -> Option<u32> {
        self.queue.iter().rev().find_map(|p| match p.kind {
            PlannedKind::Segment(s) => Some(s),
            PlannedKind::Init => None,
        })
    }

    fn build_init_cmd(&self, ctx: &PlanCtx) -> FetchCmd {
        let resource = ctx
            .asset_store
            .acquire_resource(&self.init.asset_id)
            .expect("acquire_resource for init must succeed");
        let position = Arc::clone(&self.position);
        let init_size = self.init.size;
        // Init writes don't move the playback cursor; track local offset only.
        let writer = make_writer(resource.clone());
        let variant = self.variant;
        let on_complete: OnCompleteFn = Box::new(move |_bytes_written, err| {
            finalize_init(&resource, err, variant, init_size, &position);
        });
        FetchCmd::get(self.init.url.clone())
            .cancel(Some(self.cancel.clone()))
            .writer(writer)
            .on_complete(on_complete)
    }

    fn build_seg_cmd(&self, ctx: &PlanCtx, seg_idx: u32) -> FetchCmd {
        let entry = &self.segments[seg_idx as usize];
        let resource = entry.decrypt_ctx.clone().map_or_else(
            || {
                ctx.asset_store
                    .acquire_resource(&entry.asset_id)
                    .expect("acquire_resource for segment must succeed")
            },
            |ctx_inner| {
                ctx.asset_store
                    .acquire_resource_with_ctx(&entry.asset_id, Some(ctx_inner))
                    .expect("acquire_resource_with_ctx for segment must succeed")
            },
        );
        let writer = make_writer(resource.clone());
        let variant = self.variant;
        let on_complete: OnCompleteFn = Box::new(move |_bytes_written, err| {
            finalize_seg(&resource, err, variant, seg_idx);
        });
        FetchCmd::get(entry.url.clone())
            .cancel(Some(self.cancel.clone()))
            .writer(writer)
            .on_complete(on_complete)
    }
}

fn make_writer(resource: AssetResource<DecryptContext>) -> WriterFn {
    let offset = Arc::new(AtomicU64::new(0));
    Box::new(move |chunk: &[u8]| {
        let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        resource.write_at(pos, chunk).map_err(IoError::other)
    })
}

fn finalize_init(
    resource: &AssetResource<DecryptContext>,
    err: Option<&NetError>,
    _variant: usize,
    init_size: u64,
    _position: &Arc<AtomicU64>,
) {
    if err.is_some() {
        resource.fail("init fetch failed".to_string());
        // Note: state mutation lives on HlsVariant which is owned by HlsCoord;
        // Plan 05 wires the eviction/finalize feedback path. For now the
        // resource itself records the failure.
        let _ = init_size;
        return;
    }
    let _ = resource.commit(None);
}

fn finalize_seg(
    resource: &AssetResource<DecryptContext>,
    err: Option<&NetError>,
    _variant: usize,
    _seg_idx: u32,
) {
    if err.is_some() {
        resource.fail("segment fetch failed".to_string());
        return;
    }
    let _ = resource.commit(None);
}

fn bisect_right_decode_time(segments: &[SegmentEntry], t: Duration) -> usize {
    // Standard upper-bound binary search: returns the first index whose
    // decode_time is strictly greater than `t`.
    let mut lo = 0_usize;
    let mut hi = segments.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if segments[mid].decode_time <= t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn bisect_left_byte_offset(segments: &[SegmentEntry], byte: u64) -> usize {
    let mut lo = 0_usize;
    let mut hi = segments.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if segments[mid].byte_offset < byte {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Helper for the `find_at_offset` probe — returns the seg index as a `u64`,
/// or `u64::MAX` to encode `None` on the wire.
fn find_at_offset_probe_seg(v: &HlsVariant, byte_offset: u64) -> u64 {
    v.binary_search_byte(byte_offset)
        .and_then(|i| u64::try_from(i).ok())
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_stream::dl::{Downloader, DownloaderConfig};

    use super::*;

    fn test_ctx(prefetch_budget: usize) -> PlanCtx {
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
            prefetch_budget,
        }
    }

    fn make_init(size: u64) -> InitEntry {
        let url: Url = "https://example.com/init.mp4".parse().expect("valid url");
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

    #[kithara::test]
    fn position_starts_at_zero() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
        assert_eq!(v.get_position(), 0);
    }

    #[kithara::test]
    fn advance_increments_position() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
        v.advance(64);
        assert_eq!(v.get_position(), 64);
        v.advance(36);
        assert_eq!(v.get_position(), 100);
    }

    #[kithara::test]
    fn set_position_overrides_cursor() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 400)], &ctx);
        v.advance(50);
        v.set_position(1234);
        assert_eq!(v.get_position(), 1234);
    }

    #[kithara::test]
    fn find_at_offset_below_init_returns_none() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(
            0,
            make_init(200),
            vec![make_seg(0, 200, 400), make_seg(1, 600, 400)],
            &ctx,
        );
        // Offsets below init.size (200) live in init space, not segments.
        assert!(v.find_at_offset(0).is_none());
        assert!(v.find_at_offset(199).is_none());
    }

    #[kithara::test]
    fn find_at_offset_at_init_size_returns_segment_zero() {
        let ctx = test_ctx(3);
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

    #[kithara::test]
    fn find_at_offset_mid_segment_binary_search() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(
            0,
            make_init(0),
            vec![
                make_seg(0, 200, 400),
                make_seg(1, 600, 400),
                make_seg(2, 1000, 400),
                make_seg(3, 1400, 400),
            ],
            &ctx,
        );
        let (idx, _) = v.find_at_offset(750).expect("hit");
        assert_eq!(idx, 1);
    }

    #[kithara::test]
    fn total_bytes_includes_segments() {
        let ctx = test_ctx(3);
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
        assert_eq!(v.total_bytes(), 1800);
    }

    #[kithara::test]
    fn init_byte_range_present_when_size_positive() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(200), vec![], &ctx);
        assert_eq!(v.init_byte_range(), Some(0..200));
    }

    #[kithara::test]
    fn init_byte_range_absent_when_size_zero() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(0), vec![], &ctx);
        assert!(v.init_byte_range().is_none());
    }

    #[kithara::test]
    fn descriptor_at_time_clamps_to_last() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(
            0,
            make_init(0),
            vec![
                make_seg(0, 0, 100),
                make_seg(1, 100, 100),
                make_seg(2, 200, 100),
            ],
            &ctx,
        );
        let d = v
            .descriptor_at_time(Duration::from_secs(2))
            .expect("descriptor");
        assert_eq!(d.segment_index, 1);
        // Far future clamps to last segment
        let d = v
            .descriptor_at_time(Duration::from_secs(999))
            .expect("descriptor");
        assert_eq!(d.segment_index, 2);
    }

    #[kithara::test]
    fn descriptor_after_byte_finds_next_segment() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(
            0,
            make_init(0),
            vec![
                make_seg(0, 0, 100),
                make_seg(1, 100, 100),
                make_seg(2, 200, 100),
            ],
            &ctx,
        );
        let d = v.descriptor_after_byte(50).expect("descriptor");
        assert_eq!(d.segment_index, 1);
        let d = v.descriptor_after_byte(100).expect("descriptor");
        assert_eq!(d.segment_index, 1);
    }

    #[kithara::test]
    fn rebuild_cancels_old_token_and_refills_queue() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(
            0,
            make_init(0),
            vec![
                make_seg(0, 0, 100),
                make_seg(1, 100, 100),
                make_seg(2, 200, 100),
                make_seg(3, 300, 100),
                make_seg(4, 400, 100),
                make_seg(5, 500, 100),
            ],
            &ctx,
        );
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(0),
        });
        let old_token = v.cancel.clone();
        assert!(!old_token.is_cancelled());
        v.rebuild(&ctx, 2);
        assert!(old_token.is_cancelled(), "old token must be cancelled");
        assert!(!v.cancel.is_cancelled(), "fresh token must be live");
        // Queue should be refilled from seg 2 onwards (bounded by prefetch_budget=3).
        let seg_indices: Vec<u32> = v
            .queue
            .iter()
            .filter_map(|p| match p.kind {
                PlannedKind::Segment(s) => Some(s),
                PlannedKind::Init => None,
            })
            .collect();
        assert_eq!(seg_indices, vec![2, 3, 4, 5]);
    }

    #[kithara::test]
    fn dispatch_emits_init_first_then_segments_under_budget() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(
            0,
            make_init(200),
            vec![
                make_seg(0, 200, 400),
                make_seg(1, 600, 400),
                make_seg(2, 1000, 400),
            ],
            &ctx,
        );
        v.fill_queue(&ctx, 0);
        let cmds = v.dispatch(&ctx, 10);
        // 1 init + 3 segments = 4 cmds.
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0].url, v.init.url);
        for cmd in &cmds {
            assert!(cmd.cancel.is_some(), "every cmd carries a cancel token");
        }
    }

    #[kithara::test]
    fn dispatch_respects_budget() {
        let ctx = test_ctx(5);
        let mut init = make_init(0);
        init.state = SegmentState::Loaded;
        let mut v = HlsVariant::new(
            0,
            init,
            (0..10)
                .map(|i| make_seg(i, u64::from(i) * 100, 100))
                .collect(),
            &ctx,
        );
        v.fill_queue(&ctx, 0);
        let cmds = v.dispatch(&ctx, 3);
        assert_eq!(cmds.len(), 3);
        // Remaining queue holds segments 3..=5 (fill_queue stopped at prefetch_budget=5).
        let seg_indices: Vec<u32> = v
            .queue
            .iter()
            .filter_map(|p| match p.kind {
                PlannedKind::Segment(s) => Some(s),
                PlannedKind::Init => None,
            })
            .collect();
        assert_eq!(seg_indices, vec![3, 4, 5]);
    }

    #[kithara::test]
    fn dispatch_skips_non_missing_segments() {
        let ctx = test_ctx(5);
        let mut init = make_init(0);
        init.state = SegmentState::Loaded;
        let mut v = HlsVariant::new(
            0,
            init,
            vec![
                make_seg(0, 0, 100),
                make_seg(1, 100, 100),
                make_seg(2, 200, 100),
            ],
            &ctx,
        );
        v.segments[1].state = SegmentState::Loaded;
        v.queue.clear();
        for seg in 0..3_u32 {
            v.queue.push_back(PlannedFetch {
                variant: 0,
                kind: PlannedKind::Segment(seg),
            });
        }
        let cmds = v.dispatch(&ctx, 10);
        assert_eq!(cmds.len(), 2);
        assert_eq!(v.segments[1].state, SegmentState::Loaded);
    }

    #[kithara::test]
    fn on_evict_returns_minus_one_for_init() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(0, make_init(200), vec![make_seg(0, 200, 100)], &ctx);
        v.init.state = SegmentState::Loaded;
        let asset_id = v.init.asset_id.clone();
        let res = v.on_evict(&asset_id);
        assert_eq!(res, Some(-1));
        assert_eq!(v.init.state, SegmentState::Missing);
    }

    #[kithara::test]
    fn on_evict_returns_seg_idx_for_segment() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(
            0,
            make_init(0),
            vec![make_seg(0, 0, 100), make_seg(1, 100, 100)],
            &ctx,
        );
        v.segments[1].state = SegmentState::Loaded;
        let asset_id = v.segments[1].asset_id.clone();
        let res = v.on_evict(&asset_id);
        assert_eq!(res, Some(1));
        assert_eq!(v.segments[1].state, SegmentState::Missing);
    }

    #[kithara::test]
    fn on_evict_returns_none_for_foreign_asset() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(0, make_init(0), vec![make_seg(0, 0, 100)], &ctx);
        let foreign: Url = "https://other.example.com/x.m4s".parse().expect("url");
        let foreign_key = ResourceKey::from_url(&foreign);
        let res = v.on_evict(&foreign_key);
        assert_eq!(res, None);
    }

    #[kithara::test]
    fn on_reader_advance_extends_prefetch_tail() {
        let ctx = test_ctx(3);
        let mut v = HlsVariant::new(
            0,
            make_init(0),
            (0..10)
                .map(|i| make_seg(i, u64::from(i) * 100, 100))
                .collect(),
            &ctx,
        );
        v.on_reader_advance(&ctx, 2);
        let seg_indices: Vec<u32> = v
            .queue
            .iter()
            .filter_map(|p| match p.kind {
                PlannedKind::Segment(s) => Some(s),
                PlannedKind::Init => None,
            })
            .collect();
        assert_eq!(seg_indices, vec![2, 3, 4, 5]);
    }

    #[kithara::test]
    fn skeleton_types_instantiate() {
        let ctx = test_ctx(3);
        let v = HlsVariant::new(0, make_init(200), Vec::new(), &ctx);
        assert_eq!(v.num_segments(), 0);
    }

    #[kithara::test]
    fn dispatch_drm_segment_routes_through_with_ctx() {
        let ctx = test_ctx(3);
        let mut init = make_init(0);
        init.state = SegmentState::Loaded;
        let mut seg = make_seg(0, 0, 100);
        let key = *b"0123456789abcdef";
        seg.decrypt_ctx = Some(DecryptContext::new(key, [0u8; 16]));
        let mut v = HlsVariant::new(0, init, vec![seg], &ctx);
        v.queue.push_back(PlannedFetch {
            variant: 0,
            kind: PlannedKind::Segment(0),
        });
        let cmds = v.dispatch(&ctx, 10);
        // build_seg_cmd must take the DRM branch (acquire_resource_with_ctx)
        // without panicking; the resource must exist and the cmd carries the
        // variant's cancel token.
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].cancel.is_some());
        assert_eq!(v.segments[0].state, SegmentState::Downloading);
    }
}
