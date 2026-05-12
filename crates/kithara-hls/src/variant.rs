#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::NetError;
use kithara_platform::{Mutex, RwLock, time::Duration};
use kithara_storage::ResourceExt;
use kithara_stream::{
    SegmentDescriptor,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

pub(crate) mod segment_view;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SegmentState {
    Missing = 0,
    Downloading = 1,
    Loaded = 2,
}

impl From<u8> for SegmentState {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Downloading,
            2 => Self::Loaded,
            _ => Self::Missing,
        }
    }
}

#[derive(Debug)]
struct SegmentEntry {
    url: Url,
    resource_id: ResourceKey,
    byte_offset: u64,
    size: u64,
    state: AtomicU8,
    decrypt_ctx: Option<DecryptContext>,
    decode_time: Duration,
    duration: Duration,
}

impl SegmentEntry {
    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }
}

#[derive(Debug)]
struct InitEntry {
    url: Url,
    resource_id: ResourceKey,
    size: u64,
    state: AtomicU8,
}

impl InitEntry {
    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlannedFetch {
    variant: usize,
    segment: u32,
}

pub(crate) struct PlanCtx {
    pub(crate) master_cancel: CancellationToken,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    pub(crate) prefetch_budget: usize,
}

pub(crate) struct HlsVariant {
    variant: usize,
    init: InitEntry,
    segments: Vec<SegmentEntry>,
    queue: Mutex<VecDeque<PlannedFetch>>,
    cancel: RwLock<CancellationToken>,
    position: Arc<AtomicU64>,
}

impl HlsVariant {
    #[must_use]
    fn new(variant: usize, init: InitEntry, segments: Vec<SegmentEntry>, ctx: &PlanCtx) -> Self {
        Self {
            variant,
            init,
            segments,
            queue: Mutex::new(VecDeque::new()),
            cancel: RwLock::new(ctx.master_cancel.child_token()),
            position: Arc::new(AtomicU64::new(0)),
        }
    }

    #[kithara::probe(variant = self.variant as u64, pos = self.position.load(Ordering::Acquire))]
    pub(crate) fn get_position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = find_at_offset_probe_seg(self, byte_offset)
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        let idx = self.binary_search_byte(byte_offset)?;
        let entry = &self.segments[idx];
        let idx_u32 = u32::try_from(idx).ok()?;
        Some((idx_u32, entry.byte_offset, entry.size))
    }

    #[kithara::probe(variant = self.variant as u64, total = self.total_bytes_inner())]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes_inner()
    }

    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    #[kithara::probe(variant = self.variant as u64, size = self.init.size)]
    pub(crate) fn init_byte_range(&self) -> Option<Range<u64>> {
        if self.init.size > 0 {
            Some(0..self.init.size)
        } else {
            None
        }
    }

    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn descriptor_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        self.descriptor(idx)
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
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

    /// Reissue the variant's cancel token and refill the queue starting at `from_seg`.
    /// Old token is cancelled — any `FetchCmd` holding a clone observes cancellation.
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn rebuild(&self, ctx: &PlanCtx, from_seg: u32) {
        let new_token = ctx.master_cancel.child_token();
        let old_token = {
            let mut guard = self.cancel.lock_sync_write();
            std::mem::replace(&mut *guard, new_token)
        };
        old_token.cancel();
        self.queue.lock_sync().clear();
        self.fill_queue(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, seg_at_reader)]
    pub(crate) fn on_reader_advance(&self, ctx: &PlanCtx, seg_at_reader: u32) {
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let mut queue = self.queue.lock_sync();
        if matches!(self.state_of(seg_at_reader), SegmentState::Missing)
            && !queue_contains_seg(&queue, seg_at_reader)
        {
            queue.push_front(PlannedFetch {
                variant: self.variant,
                segment: seg_at_reader,
            });
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = seg_at_reader.saturating_add(budget).min(segs_len_u32 - 1);
        let last_planned = queue.iter().next_back().map(|p| p.segment);
        let start = last_planned.map_or(seg_at_reader, |s| s.saturating_add(1));
        for seg in start..=end {
            if matches!(self.state_of(seg), SegmentState::Missing)
                && !queue_contains_seg(&queue, seg)
            {
                queue.push_back(PlannedFetch {
                    variant: self.variant,
                    segment: seg,
                });
            }
        }
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` doesn't belong to this variant.
    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if &self.init.resource_id == key {
            self.init.set_state(SegmentState::Missing);
            return Some(-1);
        }
        for (seg_idx, entry) in self.segments.iter().enumerate() {
            if &entry.resource_id == key {
                entry.set_state(SegmentState::Missing);
                return i32::try_from(seg_idx).ok();
            }
        }
        None
    }

    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn dispatch(&self, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        if matches!(self.init.state(), SegmentState::Missing) && remaining > 0 {
            self.init.set_state(SegmentState::Downloading);
            out.push(self.build_init_cmd(ctx));
            remaining -= 1;
        }
        while remaining > 0 {
            let Some(planned) = self.queue.lock_sync().pop_front() else {
                break;
            };
            let seg_idx = planned.segment;
            let Some(entry) = self.segments.get(seg_idx as usize) else {
                continue;
            };
            if !matches!(entry.state(), SegmentState::Missing) {
                continue;
            }
            entry.set_state(SegmentState::Downloading);
            out.push(self.build_seg_cmd(ctx, seg_idx));
            remaining -= 1;
        }
        out
    }

    pub(crate) fn cancel_handle(&self) -> CancellationToken {
        self.cancel.lock_sync_read().clone()
    }

    pub(crate) fn cancel(&self) {
        self.cancel.lock_sync_read().cancel();
    }

    pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64> {
        self.segments.get(seg_idx as usize).map(|e| e.byte_offset)
    }

    pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        u32::try_from(idx).ok()
    }

    fn binary_search_byte(&self, byte_offset: u64) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
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

    fn fill_queue(&self, ctx: &PlanCtx, from_seg: u32) {
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = from_seg.saturating_add(budget).min(segs_len_u32 - 1);
        let mut queue = self.queue.lock_sync();
        for seg in from_seg..=end {
            if matches!(self.state_of(seg), SegmentState::Missing) {
                queue.push_back(PlannedFetch {
                    variant: self.variant,
                    segment: seg,
                });
            }
        }
    }

    fn state_of(&self, seg_idx: u32) -> SegmentState {
        self.segments
            .get(seg_idx as usize)
            .map_or(SegmentState::Missing, SegmentEntry::state)
    }

    fn build_init_cmd(&self, ctx: &PlanCtx) -> FetchCmd {
        let resource = ctx
            .asset_store
            .acquire_resource(&self.init.resource_id)
            .expect("acquire_resource for init must succeed");
        let position = Arc::clone(&self.position);
        let init_size = self.init.size;
        let writer = make_writer(resource.clone());
        let variant = self.variant;
        let on_complete: OnCompleteFn = Box::new(move |_bytes_written, err| {
            finalize_init(&resource, err, variant, init_size, &position);
        });
        FetchCmd::get(self.init.url.clone())
            .cancel(Some(self.cancel_handle()))
            .writer(writer)
            .on_complete(on_complete)
    }

    fn build_seg_cmd(&self, ctx: &PlanCtx, seg_idx: u32) -> FetchCmd {
        let entry = &self.segments[seg_idx as usize];
        let resource = entry.decrypt_ctx.clone().map_or_else(
            || {
                ctx.asset_store
                    .acquire_resource(&entry.resource_id)
                    .expect("acquire_resource for segment must succeed")
            },
            |ctx_inner| {
                ctx.asset_store
                    .acquire_resource_with_ctx(&entry.resource_id, Some(ctx_inner))
                    .expect("acquire_resource_with_ctx for segment must succeed")
            },
        );
        let writer = make_writer(resource.clone());
        let variant = self.variant;
        let on_complete: OnCompleteFn = Box::new(move |_bytes_written, err| {
            finalize_seg(&resource, err, variant, seg_idx);
        });
        FetchCmd::get(entry.url.clone())
            .cancel(Some(self.cancel_handle()))
            .writer(writer)
            .on_complete(on_complete)
    }
}

fn queue_contains_seg(queue: &VecDeque<PlannedFetch>, seg_idx: u32) -> bool {
    queue.iter().any(|p| p.segment == seg_idx)
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

fn find_at_offset_probe_seg(v: &HlsVariant, byte_offset: u64) -> u64 {
    v.binary_search_byte(byte_offset)
        .and_then(|i| u64::try_from(i).ok())
        .unwrap_or(u64::MAX)
}
