use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kithara_platform::RwLock;

use super::SegmentEntry;

/// One coherent coordinate frame: the cross-variant byte-address-space
/// geometry. Held behind a single `RwLock` so every reader observes the
/// shift, served range, init seed and offset table from the SAME
/// activation — the split-lock torn read (shift from one activation,
/// served bounds from the next) is impossible by construction.
struct Frame {
    /// Virtual = natural + `byte_shift`. Pins a switched variant's
    /// `from_seg` onto the outgoing variant's segment boundary so the
    /// combined byte stream stays contiguous and fMP4 box addresses align.
    byte_shift: i64,
    /// First media segment served (inclusive) in the combined stream.
    served_from: u32,
    /// Last media segment served (exclusive).
    served_until: u32,
    /// Frozen `init.size` seed for `recompute` on switched variants; `0`
    /// means "use the current `init.size`" (initial-activation path).
    init_seed: u64,
    /// Cumulative natural byte offsets for media segments, seeded with the
    /// init prefix length.
    offsets: Vec<u64>,
}

/// The produce-core's lock-free EOF view, computed from a `Frame` under the
/// write-lock and published into [`Layout`]'s atomics. Both fields move
/// together so the byte-EOF gates never see `total` from one mutation and
/// `sizes_complete` from another.
#[derive(Clone, Copy)]
struct FrameSnapshot {
    total: u64,
    sizes_complete: bool,
}

impl Frame {
    fn recompute(&mut self, init_size: u64, segments: &[SegmentEntry]) {
        self.offsets.resize(segments.len(), 0);
        let mut cum = if self.init_seed > 0 {
            self.init_seed
        } else {
            init_size
        };
        for (i, s) in segments.iter().enumerate() {
            self.offsets[i] = cum;
            cum += s.size.load(Ordering::Acquire);
        }
    }

    fn segment_byte_offset(&self, idx: u32) -> Option<u64> {
        let natural = self.offsets.get(idx as usize).copied()?;
        let virt = i64::try_from(natural).ok()?.checked_add(self.byte_shift)?;
        u64::try_from(virt).ok()
    }

    fn bisect_left(&self, byte: u64) -> usize {
        let mut lo = 0_usize;
        let mut hi = self.offsets.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.offsets[mid] < byte {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Binary search in **natural** byte space — no shift, no served-range
    /// gate. Returns `(idx, natural_offset, size)`.
    fn find_natural(&self, byte: u64, segments: &[SegmentEntry]) -> Option<(u32, u64, u64)> {
        let mut lo = 0_usize;
        let mut hi = self.offsets.len();
        let mut hit = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.offsets[mid];
            let size = segments[mid].size.load(Ordering::Acquire);
            if byte < off {
                hi = mid;
            } else if byte >= off + size {
                lo = mid + 1;
            } else {
                hit = Some((mid, off, size));
                break;
            }
        }
        let (mid, off, size) = hit?;
        Some((u32::try_from(mid).ok()?, off, size))
    }

    /// Reader-facing lookup in **virtual** byte space. Subtracts
    /// `byte_shift`, runs the natural-space search, then gates the result
    /// against `[served_from, served_until)` and re-shifts. All four fields
    /// (`byte_shift`, `offsets`, `served_from`, `served_until`) come from
    /// the same locked frame.
    fn find_virtual(
        &self,
        byte_virtual: u64,
        segments: &[SegmentEntry],
    ) -> Option<(u32, u64, u64)> {
        let byte_natural = i64::try_from(byte_virtual)
            .ok()?
            .checked_sub(self.byte_shift)?;
        if byte_natural < 0 {
            return None;
        }
        let byte_natural = u64::try_from(byte_natural).ok()?;
        let (idx, off_nat, size) = self.find_natural(byte_natural, segments)?;
        if idx < self.served_from || idx >= self.served_until {
            return None;
        }
        let off_virtual =
            u64::try_from(i64::try_from(off_nat).ok()?.checked_add(self.byte_shift)?).ok()?;
        Some((idx, off_virtual, size))
    }

    /// Whether every served segment `[served_from, served_until)` has a
    /// known (non-zero) byte size. A `0` size means "unknown" — the size
    /// estimate's HEAD failed (or has not run) for that segment, so its
    /// bytes are unaccounted-for in `total_bytes`. Until this holds,
    /// `total_bytes` is a lower bound, NOT the authoritative stream end, and
    /// must not be used to mint EOF (an in-range offset would falsely look
    /// past-the-end against the under-count).
    fn sizes_complete(&self, segments: &[SegmentEntry]) -> bool {
        let start = self.served_from as usize;
        let end = (self.served_until as usize).min(segments.len());
        if start >= end {
            // No served media segments (init-only / empty): the offset table
            // alone bounds the stream; treat as complete.
            return true;
        }
        segments[start..end]
            .iter()
            .all(|s| s.size.load(Ordering::Acquire) > 0)
    }

    /// Materialise the lock-free EOF snapshot (`total_bytes` +
    /// `sizes_complete`) in one read so the caller can drop the write-lock
    /// guard before publishing the atomics.
    fn snapshot(&self, segments: &[SegmentEntry], init_size: u64) -> FrameSnapshot {
        FrameSnapshot {
            total: self.total_bytes(segments, init_size),
            sizes_complete: self.sizes_complete(segments),
        }
    }

    fn total_bytes(&self, segments: &[SegmentEntry], init_size: u64) -> u64 {
        let seg_size = |idx: usize| {
            segments
                .get(idx)
                .map_or(0, |s| s.size.load(Ordering::Acquire))
        };
        let last_idx = (self.served_until as usize).saturating_sub(1);
        let natural = if let (Some(off), Some(_seg)) =
            (self.offsets.get(last_idx).copied(), segments.get(last_idx))
        {
            off + seg_size(last_idx)
        } else if let Some(idx) = segments.len().checked_sub(1)
            && let Some(off) = self.offsets.get(idx).copied()
        {
            off + seg_size(idx)
        } else {
            init_size
        };
        let adjusted = i64::try_from(natural)
            .ok()
            .and_then(|n| n.checked_add(self.byte_shift));
        match adjusted {
            Some(v) if v >= 0 => u64::try_from(v).unwrap_or(0),
            _ => 0,
        }
    }
}

/// Coherent owner of the five coordinate fields. Reads take the shared
/// lock and see one activation frame; the `activate_*` / `reset` /
/// `apply_commit` mutators take the exclusive lock and publish a new frame
/// atomically.
///
/// `total` is a lock-free snapshot of `total_bytes`, republished from the
/// frame at the end of every write-lock mutation. The produce-core read path
/// (`wait_range` / `range_ready` / `read_at`) loads it without taking the
/// lock — closing the contended `RwLock` spin the `rtsan` lane flagged on the
/// decode core.
/// Inputs to [`Layout::activate_with_shift`]: pin `from_seg` at
/// `seg_boundary` in virtual space, using `init_size` for offset recompute.
#[derive(Clone, Copy)]
pub(super) struct ActivateParams {
    pub(super) from_seg: u32,
    pub(super) seg_boundary: u64,
    pub(super) init_size: u64,
}

pub(super) struct Layout {
    frame: RwLock<Frame>,
    total: AtomicU64,
    /// Lock-free snapshot of [`Frame::sizes_complete`], republished from the
    /// frame at the end of every write-lock mutation. The produce-core EOF
    /// gates (`wait_range` / `phase_at` / `read_at`) read it without taking
    /// the lock, alongside `total`.
    sizes_complete: AtomicBool,
}

impl Layout {
    pub(super) fn new(init_size: u64, segments: &[SegmentEntry]) -> Self {
        let num = u32::try_from(segments.len()).unwrap_or(u32::MAX);
        let mut frame = Frame {
            byte_shift: 0,
            served_from: 0,
            served_until: num,
            init_seed: 0,
            offsets: Vec::new(),
        };
        frame.recompute(init_size, segments);
        let snapshot = frame.snapshot(segments, init_size);
        Self {
            frame: RwLock::new(frame),
            total: AtomicU64::new(snapshot.total),
            sizes_complete: AtomicBool::new(snapshot.sizes_complete),
        }
    }

    pub(super) fn served_from(&self) -> u32 {
        self.frame.read().served_from
    }

    pub(super) fn set_served_until(&self, until: u32, segments: &[SegmentEntry], init_size: u64) {
        let mut frame = self.frame.write();
        frame.served_until = until;
        let snapshot = frame.snapshot(segments, init_size);
        drop(frame);
        self.republish(snapshot);
    }

    /// Republish the lock-free `total` + `sizes_complete` snapshot computed
    /// from the just-mutated frame. The two atomics together form the
    /// produce-core EOF view, stored in one place at the tail of every
    /// mutation. The frame guard is dropped before this call (the snapshot is
    /// already materialised under-lock), so the lock is held no longer than
    /// the mutation itself.
    fn republish(&self, snapshot: FrameSnapshot) {
        self.total.store(snapshot.total, Ordering::Release);
        self.sizes_complete
            .store(snapshot.sizes_complete, Ordering::Release);
    }

    /// Coherent "is this variant historical?" check: `served_from` and
    /// `served_until` are read under one lock, closing the coord-level
    /// torn read between two separate accessor calls.
    pub(super) fn is_shrunk(&self, num_segments: u32) -> bool {
        let frame = self.frame.read();
        frame.served_from > 0 || frame.served_until < num_segments
    }

    pub(super) fn natural_offset(&self, idx: usize) -> Option<u64> {
        self.frame.read().offsets.get(idx).copied()
    }

    pub(super) fn segment_byte_offset(&self, idx: u32) -> Option<u64> {
        self.frame.read().segment_byte_offset(idx)
    }

    pub(super) fn bisect_left(&self, byte: u64) -> usize {
        self.frame.read().bisect_left(byte)
    }

    pub(super) fn find_natural(
        &self,
        byte: u64,
        segments: &[SegmentEntry],
    ) -> Option<(u32, u64, u64)> {
        self.frame.read().find_natural(byte, segments)
    }

    pub(super) fn find_at_offset(
        &self,
        byte_virtual: u64,
        segments: &[SegmentEntry],
    ) -> Option<(u32, u64, u64)> {
        self.frame.read().find_virtual(byte_virtual, segments)
    }

    /// Lock-free `total_bytes` read for the produce-core. Returns the value
    /// published by the most recent write-lock mutation — never takes the
    /// frame lock, so it cannot spin on a concurrent activation/commit.
    pub(super) fn total_bytes(&self) -> u64 {
        self.total.load(Ordering::Acquire)
    }

    /// Lock-free `sizes_complete` read for the produce-core EOF gates. `false`
    /// means at least one served segment's size is still unknown, so
    /// [`Self::total_bytes`] is a lower bound and must not mint EOF.
    pub(super) fn sizes_complete(&self) -> bool {
        self.sizes_complete.load(Ordering::Acquire)
    }

    pub(super) fn clear_init_seed(&self) {
        self.frame.write().init_seed = 0;
    }

    /// Pin `from_seg` at `seg_boundary` in virtual space and serve
    /// `[from_seg, num_segments)`. The seed, offset recompute, shift and
    /// served bounds are published under one write-lock so no reader sees a
    /// half-built frame.
    pub(super) fn activate_with_shift(&self, params: ActivateParams, segments: &[SegmentEntry]) {
        let ActivateParams {
            from_seg,
            seg_boundary,
            init_size,
        } = params;
        let num = u32::try_from(segments.len()).unwrap_or(u32::MAX);
        let mut frame = self.frame.write();
        frame.init_seed = init_size.max(1);
        frame.recompute(init_size, segments);
        let natural = frame
            .offsets
            .get(from_seg as usize)
            .copied()
            .unwrap_or_else(|| frame.total_bytes(segments, init_size));
        let shift = i64::try_from(seg_boundary)
            .ok()
            .zip(i64::try_from(natural).ok())
            .and_then(|(v, n)| v.checked_sub(n))
            .unwrap_or(0);
        frame.byte_shift = shift;
        frame.served_from = from_seg;
        frame.served_until = num;
        let snapshot = frame.snapshot(segments, init_size);
        drop(frame);
        self.republish(snapshot);
    }

    /// Collapse to a single-variant layout: `byte_shift = 0`,
    /// `served = [0, num_segments)`, offsets recomputed from the existing
    /// seed.
    pub(super) fn reset(&self, init_size: u64, segments: &[SegmentEntry]) {
        let num = u32::try_from(segments.len()).unwrap_or(u32::MAX);
        let mut frame = self.frame.write();
        frame.byte_shift = 0;
        frame.served_from = 0;
        frame.served_until = num;
        frame.recompute(init_size, segments);
        let snapshot = frame.snapshot(segments, init_size);
        drop(frame);
        self.republish(snapshot);
    }

    /// Apply a settled size and recompute offsets under one write-lock.
    /// `store` performs the caller-owned size store (init or segment atom)
    /// and returns the post-store `init_size` to seed the recompute — so a
    /// reader never observes a new size against a stale offset table.
    pub(super) fn apply_commit(&self, segments: &[SegmentEntry], store: impl FnOnce() -> u64) {
        let mut frame = self.frame.write();
        let init_size = store();
        frame.recompute(init_size, segments);
        let snapshot = frame.snapshot(segments, init_size);
        drop(frame);
        self.republish(snapshot);
    }
}
