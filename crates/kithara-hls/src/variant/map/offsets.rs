use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

use arc_swap::ArcSwap;
use kithara_platform::sync::{Arc, Mutex, Retired};
use kithara_test_utils::kithara;

use crate::segment::Segment;

/// One coherent coordinate frame: the cross-variant byte-address-space
/// geometry. Published as an immutable [`ArcSwap`] snapshot so every reader
/// observes the shift, served range, init seed and offset table from the SAME
/// activation — the split-lock torn read (shift from one activation, served
/// bounds from the next) is impossible by construction. Off-RT writers
/// serialize through [`Layout::write_lock`] (load-clone-mutate-store) so no
/// concurrent writer drops an update; the produce-core reads load the snapshot
/// lock-free and allocation-free.
#[derive(Clone)]
struct Frame {
    /// Cumulative natural byte offsets for media segments, seeded with the
    /// init prefix length.
    offsets: Vec<u64>,
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
}

/// The produce-core's lock-free EOF view, computed from a `Frame` under the
/// write-lock and published into [`Layout`]'s atomics. Both fields move
/// together so the byte-EOF gates never see `total` from one mutation and
/// `sizes_complete` from another.
#[derive(Clone, Copy)]
struct FrameSnapshot {
    sizes_complete: bool,
    total: u64,
}

impl Frame {
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
    fn find_natural(&self, byte: u64, segments: &[Segment]) -> Option<(u32, u64, u64)> {
        let mut lo = 0_usize;
        let mut hi = self.offsets.len();
        let mut hit = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.offsets[mid];
            let size = segments[mid].len();
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
    /// the same published frame.
    fn find_virtual(&self, byte_virtual: u64, segments: &[Segment]) -> Option<(u32, u64, u64)> {
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

    #[kithara::probe]
    fn recompute(&mut self, init_size: u64, segments: &[Segment]) {
        self.offsets.resize(segments.len(), 0);
        let mut cum = if self.init_seed > 0 {
            self.init_seed
        } else {
            init_size
        };
        for (i, s) in segments.iter().enumerate() {
            self.offsets[i] = cum;
            cum += s.len();
        }
    }

    fn segment_byte_offset(&self, idx: u32) -> Option<u64> {
        let natural = self.offsets.get(idx as usize).copied()?;
        let virt = i64::try_from(natural).ok()?.checked_add(self.byte_shift)?;
        u64::try_from(virt).ok()
    }

    /// Whether every served segment `[served_from, served_until)` has an
    /// exact byte size. Non-exact placeholders may contribute to routing
    /// geometry, but until this holds `total_bytes` is not the authoritative
    /// stream end and must not be used to mint EOF.
    ///
    /// Treats an init-only or empty stream as complete: with no served segments, the offset table
    /// alone bounds the stream.
    fn sizes_complete(&self, segments: &[Segment]) -> bool {
        let start = self.served_from as usize;
        let end = (self.served_until as usize).min(segments.len());
        if start >= end {
            return true;
        }
        segments[start..end].iter().all(|s| s.size().is_exact())
    }

    /// Materialise the lock-free EOF snapshot (`total_bytes` +
    /// `sizes_complete`) in one read so the caller can publish the atomics
    /// alongside the new frame snapshot.
    fn snapshot(&self, segments: &[Segment], init_size: u64) -> FrameSnapshot {
        FrameSnapshot {
            total: self.total_bytes(segments, init_size),
            sizes_complete: self.sizes_complete(segments),
        }
    }

    fn total_bytes(&self, segments: &[Segment], init_size: u64) -> u64 {
        let seg_size = |idx: usize| segments.get(idx).map_or(0, Segment::len);
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

/// Coherent owner of the five coordinate fields. Reads `load` the published
/// frame snapshot lock-free; the `activate_*` / `reset` / `apply_commit`
/// mutators serialize through `write_lock`, clone the live frame, mutate the
/// owned copy, and `store` it back as a fresh immutable snapshot.
///
/// `total` is a lock-free snapshot of `total_bytes`, republished from the
/// frame at the end of every write-lock mutation. The produce-core read path
/// (`wait_range` / `range_ready` / `read_at`) loads it without taking the
/// lock — closing the contended frame-lock spin the `rtsan` lane flagged on the
/// decode core.
pub(in crate::variant) struct Layout {
    /// Immutable published geometry. Readers `load` it lock-free and
    /// allocation-free; writers swap in a fresh `Arc<Frame>` under `write_lock`.
    frame: ArcSwap<Frame>,
    /// Lock-free snapshot of [`Frame::sizes_complete`], republished from the
    /// frame at the end of every write-lock mutation. The produce-core EOF
    /// gates (`wait_range` / `phase_at` / `read_at`) read it without taking
    /// the lock, alongside `total`.
    sizes_complete: AtomicBool,
    /// Odd while a writer is publishing size atoms, geometry, and the EOF
    /// snapshot; even while the published view is stable. Exact-size gates
    /// sample it without spinning so they cannot combine a new size with an
    /// old frame.
    publication_seq: AtomicU64,
    total: AtomicU64,
    /// Serializes off-RT writers; never taken on the produce-core read
    /// path. Payload: frames displaced by a swap, held until quiesced
    /// (`strong_count == 1`) so reclamation stays on the writer and a
    /// reader guard drop is a pure decrement.
    write_lock: Mutex<Retired<Frame>>,
}

impl Layout {
    pub(in crate::variant) fn new(init_size: u64, segments: &[Segment]) -> Self {
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
            frame: ArcSwap::from(Arc::new(frame)),
            write_lock: Mutex::new(Retired::default()),
            total: AtomicU64::new(snapshot.total),
            sizes_complete: AtomicBool::new(snapshot.sizes_complete),
            publication_seq: AtomicU64::new(0),
        }
    }

    /// Apply a settled size and recompute offsets in one frame swap. `store`
    /// performs the caller-owned size store (init or segment atom) and returns
    /// the post-store `init_size` to seed the recompute — so a reader never
    /// observes a new size against a stale offset table. The store runs under
    /// `write_lock` so it serializes with the frame mutation.
    pub(in crate::variant) fn apply_commit(
        &self,
        segments: &[Segment],
        store: impl FnOnce() -> u64,
    ) {
        self.apply_commit_with(segments, store, || {});
    }

    #[cfg(test)]
    pub(in crate::variant) fn apply_commit_before_publish(
        &self,
        segments: &[Segment],
        store: impl FnOnce() -> u64,
        before_publish: impl FnOnce(),
    ) {
        self.apply_commit_with(segments, store, before_publish);
    }

    fn apply_commit_with(
        &self,
        segments: &[Segment],
        store: impl FnOnce() -> u64,
        before_publish: impl FnOnce(),
    ) {
        self.mutate_frame(
            segments,
            || {
                let init_size = store();
                before_publish();
                init_size
            },
            |frame, init_size| frame.recompute(init_size, segments),
        );
    }

    fn begin_publication(&self) {
        self.publication_seq.fetch_add(1, Ordering::AcqRel);
    }

    fn finish_publication(&self) {
        self.publication_seq.fetch_add(1, Ordering::Release);
    }

    /// Publishes coordinate metadata against one stable layout without rebuilding its table.
    pub(in crate::variant) fn with_metadata_update<T>(&self, update: impl FnOnce() -> T) -> T {
        let _writer = self.write_lock.lock();
        self.begin_publication();
        let result = update();
        self.finish_publication();
        result
    }

    /// True when the frame is already the canonical single-variant
    /// full-range layout (`byte_shift == 0`, `served = [0, num)`, no
    /// `init_seed`, offsets sized to `segments`) AND every served size is
    /// already exact. In that state a [`Self::reset`] would recompute the
    /// identical offset table; the skip decision lives in
    /// `HlsVariant::layout_seek_invariant`, which also weighs the parked
    /// settles this type cannot see. A shifted/shrunk/size-incomplete
    /// frame returns `false` — cross-variant and partial-download seeks
    /// keep their reset.
    pub(in crate::variant) fn is_canonical_complete(&self, segments: &[Segment]) -> bool {
        if !self.sizes_complete.load(Ordering::Acquire) {
            return false;
        }
        let num = u32::try_from(segments.len()).unwrap_or(u32::MAX);
        let frame = self.frame.load();
        frame.byte_shift == 0
            && frame.served_from == 0
            && frame.served_until == num
            && frame.init_seed == 0
            && frame.offsets.len() == segments.len()
    }

    /// Serialized load-clone-mutate-store for off-RT writers. Takes
    /// `write_lock`, runs the caller-owned `store` (size stores + the
    /// post-store `init_size`) under it, clones the live frame, applies `f`
    /// to the owned copy, materialises the EOF snapshot, swaps the new frame
    /// in, and republishes the atomics. Concurrent writers cannot lose an
    /// update because each runs the whole cycle under `write_lock`.
    fn mutate_frame(
        &self,
        segments: &[Segment],
        store: impl FnOnce() -> u64,
        f: impl FnOnce(&mut Frame, u64),
    ) {
        let mut retired = self.write_lock.lock();
        self.begin_publication();
        let init_size = store();
        let mut frame = (**self.frame.load()).clone();
        f(&mut frame, init_size);
        let snapshot = frame.snapshot(segments, init_size);
        let displaced = self.frame.swap(Arc::new(frame));
        self.republish(snapshot);
        self.finish_publication();
        retired.retire(displaced);
    }

    pub(in crate::variant) fn natural_offset(&self, idx: usize) -> Option<u64> {
        self.frame.load().offsets.get(idx).copied()
    }

    /// Republish the lock-free `total` + `sizes_complete` snapshot computed
    /// from the just-published frame. The two atomics together form the
    /// produce-core EOF view, stored in one place at the tail of every
    /// mutation.
    fn republish(&self, snapshot: FrameSnapshot) {
        self.total.store(snapshot.total, Ordering::Release);
        self.sizes_complete
            .store(snapshot.sizes_complete, Ordering::Release);
    }

    /// Collapse to a single-variant layout: `byte_shift = 0`,
    /// `served = [0, num_segments)`, offsets recomputed from the existing
    /// seed. `store` runs under the write lock before the recompute — the
    /// caller applies size stores deferred while the old byte space was
    /// live (see `HlsVariant::apply_commit`) and returns the post-store
    /// `init_size`, so the fresh frame and those sizes publish atomically.
    pub(in crate::variant) fn reset(&self, segments: &[Segment], store: impl FnOnce() -> u64) {
        let num = u32::try_from(segments.len()).unwrap_or(u32::MAX);
        self.mutate_frame(segments, store, |frame, init_size| {
            frame.byte_shift = 0;
            frame.served_from = 0;
            frame.served_until = num;
            frame.init_seed = 0;
            frame.recompute(init_size, segments);
        });
    }

    /// Lock-free `sizes_complete` read for the produce-core EOF gates. `false`
    /// means at least one served segment's size is still unknown, so
    /// [`Self::total_bytes`] is a lower bound and must not mint EOF.
    pub(in crate::variant) fn sizes_complete(&self) -> bool {
        self.sizes_complete.load(Ordering::Acquire)
    }

    /// Lock-free `total_bytes` read for the produce-core. Returns the value
    /// published by the most recent write-lock mutation — never takes the
    /// frame lock, so it cannot spin on a concurrent activation/commit.
    pub(in crate::variant) fn total_bytes(&self) -> u64 {
        self.total.load(Ordering::Acquire)
    }

    /// Run one lock-free read against a stable layout publication. A writer
    /// in progress or a publication change returns `None`; callers keep their
    /// metadata gate closed and retry on the next tick.
    pub(in crate::variant) fn try_published<T>(
        &self,
        read: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let before = self.publication_seq.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }
        let value = read()?;
        fence(Ordering::Acquire);
        let after = self.publication_seq.load(Ordering::Acquire);
        (before == after).then_some(value)
    }

    delegate::delegate! {
        to self.frame.load() {
            pub(in crate::variant) fn bisect_left(&self, byte: u64) -> usize;
            #[call(find_virtual)]
            pub(in crate::variant) fn find_at_offset(
                &self,
                byte_virtual: u64,
                segments: &[Segment],
            ) -> Option<(u32, u64, u64)>;
            pub(in crate::variant) fn find_natural(&self, byte: u64, segments: &[Segment]) -> Option<(u32, u64, u64)>;
            pub(in crate::variant) fn segment_byte_offset(&self, idx: u32) -> Option<u64>;
            #[field]
            pub(in crate::variant) fn served_from(&self) -> u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Weak;

    use super::*;

    #[kithara::test]
    fn displaced_frame_is_freed_by_the_writer() {
        let layout = Layout::new(0, &[]);
        let displaced: Weak<Frame> = Arc::downgrade(&layout.frame.load_full());

        layout.apply_commit(&[], || 0);
        assert!(
            displaced.upgrade().is_some(),
            "a racing reader guard may still resolve to the displaced frame"
        );

        layout.apply_commit(&[], || 0);
        assert!(
            displaced.upgrade().is_none(),
            "the next mutation reclaims quiesced frames"
        );
    }

    #[kithara::test]
    fn mutate_frame_retires_the_displaced_frame() {
        let layout = Layout::new(0, &[]);
        let displaced: Weak<Frame> = Arc::downgrade(&layout.frame.load_full());

        layout.mutate_frame(&[], || 0, |_, _| {});
        assert!(displaced.upgrade().is_some());
    }

    #[kithara::test]
    fn outstanding_reader_blocks_reclamation() {
        let layout = Layout::new(0, &[]);
        let reader: Arc<Frame> = layout.frame.load_full();
        let displaced = Arc::downgrade(&reader);

        layout.apply_commit(&[], || 0);
        layout.apply_commit(&[], || 0);
        assert!(
            displaced.upgrade().is_some(),
            "an outstanding reader blocks reclamation"
        );

        drop(reader);
        layout.apply_commit(&[], || 0);
        assert!(displaced.upgrade().is_none());
    }
}
