use std::{
    hint::spin_loop,
    sync::atomic::{AtomicU32, AtomicU64, Ordering, fence},
};

use super::seqlock::AnchorEntry;

/// A generation-tagged MULTI-writer seqlock cell holding `{segment, anchor}`
/// plus a present/absent generation. Unlike [`SeqAnchorCell`](super::seqlock::SeqAnchorCell)
/// (a single on-core body writer), the exact-seek demand is body-written from
/// BOTH the produce-core seek path (`seek_time_anchor`) and the off-RT
/// downloader seek-epoch reset (`rebuild_at_time`), with no lock shared between
/// them. The version is therefore acquired with a CAS (even -> odd): the two
/// writers serialize, the loser retries — writes are short (five atomic stores,
/// no alloc/lock/log) and rare. The RT read path never spins on a
/// write-in-flight: an odd or changed version returns `None` (not-ready) for
/// this poll and relies on the existing level-triggered re-poll
/// (`SchedulerWake` / `WAITING_TIMEOUT` / next `read_at`) to observe the demand
/// a tick later — the non-blocking analog of the original `Mutex`'s blocking
/// wait. `active` is the present generation (0 = `None`), published *inside* the
/// version critical section so two writers' publishes can never lost-update
/// each other; off-RT consumers may CAS it to 0. See the crate `CONTEXT.md`
/// "Seek-state primitives".
pub(super) struct CasAnchorCell {
    /// Seqlock version: even = stable, odd = a writer owns the body. Acquired
    /// with a CAS so two concurrent writers cannot both hold it.
    version: AtomicU32,
    /// Present generation: 0 = absent, otherwise the current monotonic
    /// generation. Written only under the version lock.
    active: AtomicU64,
    /// Monotonic generation source.
    next_gen: AtomicU64,
    segment: AtomicU32,
    anchor: AtomicU64,
}

impl CasAnchorCell {
    pub(super) const fn new() -> Self {
        Self {
            version: AtomicU32::new(0),
            active: AtomicU64::new(0),
            next_gen: AtomicU64::new(0),
            segment: AtomicU32::new(0),
            anchor: AtomicU64::new(0),
        }
    }

    pub(super) fn load(&self) -> Option<AnchorEntry> {
        let start = self.version.load(Ordering::Acquire);
        if start & 1 != 0 {
            // A writer owns the body: bail to not-ready for this poll instead of
            // spinning. The level-triggered re-poll observes the demand a tick
            // later.
            return None;
        }
        let generation = self.active.load(Ordering::Acquire);
        if generation == 0 {
            return None;
        }
        let segment = self.segment.load(Ordering::Relaxed);
        let anchor = self.anchor.load(Ordering::Relaxed);
        // Pin the Relaxed body loads before the re-validation: on weak-memory
        // targets (AArch64) they could otherwise sink past the version/`active`
        // recheck and accept a torn `{segment, anchor}` from a newer writer.
        fence(Ordering::Acquire);
        // Version sandwich: an unchanged even version proves no writer touched
        // body or `active` across the read, so the snapshot is coherent.
        if self.version.load(Ordering::Acquire) != start {
            return None;
        }
        // Reject a snapshot a concurrent `clear`/`take_if` already retired.
        if self.active.load(Ordering::Acquire) != generation {
            return None;
        }
        Some(AnchorEntry {
            generation,
            segment,
            anchor,
        })
    }

    /// Multi-writer publish. Acquires the version with a CAS (even -> odd); a
    /// racing writer retries. `active` and the body are written under the lock,
    /// so two writers' publishes serialize and never lost-update each other.
    pub(super) fn set(&self, segment: u32, anchor: u64) {
        let held = loop {
            let cur = self.version.load(Ordering::Acquire);
            if cur & 1 == 0
                && self
                    .version
                    .compare_exchange_weak(
                        cur,
                        cur.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                break cur.wrapping_add(1);
            }
            spin_loop();
        };
        let generation = self.next_gen();
        // Hide the demand for the body write so a stale `take_if(old)` cannot
        // match mid-rewrite.
        self.active.store(0, Ordering::Release);
        self.segment.store(segment, Ordering::Relaxed);
        self.anchor.store(anchor, Ordering::Relaxed);
        self.active.store(generation, Ordering::Release);
        // Release the version lock (odd -> even).
        self.version.store(held.wrapping_add(1), Ordering::Release);
    }

    pub(super) fn clear(&self) {
        self.active.store(0, Ordering::Release);
    }

    /// Consume the demand iff it is still generation `generation`. Safe from any
    /// thread; the CAS picks a single winner across concurrent completers.
    pub(super) fn take_if(&self, generation: u64) -> bool {
        self.active
            .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    fn next_gen(&self) -> u64 {
        let generation = self
            .next_gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if generation == 0 {
            // 2^64 SETs is unreachable in practice; keep 0 reserved for absent.
            self.next_gen
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1)
        } else {
            generation
        }
    }
}
