use std::{
    hint::spin_loop,
    sync::atomic::{AtomicU32, AtomicU64, Ordering, fence},
};

/// Single-writer seqlock version counter: even = stable, odd = write in
/// progress. The produce-core SET path is the only body writer; off-RT
/// readers (settle) and on-core readers retry on a torn snapshot. See the
/// crate `CONTEXT.md` "Seek-state primitives".
struct SeqVersion {
    version: AtomicU32,
}

impl SeqVersion {
    const fn new() -> Self {
        Self {
            version: AtomicU32::new(0),
        }
    }

    fn begin(&self) {
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    fn end(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }

    fn read<T>(&self, f: impl Fn() -> T) -> T {
        loop {
            let start = self.version.load(Ordering::Acquire);
            if start & 1 == 0 {
                let out = f();
                // Pin the Relaxed body loads in `f` before the version recheck:
                // AArch64 load-load reordering could otherwise accept a torn
                // snapshot from a concurrent writer.
                fence(Ordering::Acquire);
                if self.version.load(Ordering::Acquire) == start {
                    return out;
                }
            }
            spin_loop();
        }
    }
}

/// A generation-tagged seqlock cell holding `{segment, anchor}` plus a
/// present/absent generation. The body (`segment`/`anchor`) is written only by
/// the on-core SET path (single writer); `active` is the present generation
/// (0 = `None`), which off-RT consumers may CAS to 0. Reads are lock-free and
/// allocation-free on both produce-core and off-RT threads.
pub(super) struct SeqAnchorCell {
    segment: AtomicU32,
    /// Present generation: 0 = absent, otherwise the current monotonic generation.
    active: AtomicU64,
    anchor: AtomicU64,
    /// Monotonic generation source. Bumped only by the on-core SET path.
    next_gen: AtomicU64,
    seq: SeqVersion,
}

#[derive(Clone, Copy)]
pub(super) struct AnchorEntry {
    pub(super) segment: u32,
    pub(super) anchor: u64,
    pub(super) generation: u64,
}

impl SeqAnchorCell {
    pub(super) const fn new() -> Self {
        Self {
            seq: SeqVersion::new(),
            active: AtomicU64::new(0),
            next_gen: AtomicU64::new(0),
            segment: AtomicU32::new(0),
            anchor: AtomicU64::new(0),
        }
    }

    pub(super) fn clear(&self) {
        self.active.store(0, Ordering::Release);
    }

    pub(super) fn load(&self) -> Option<AnchorEntry> {
        loop {
            let generation = self.active.load(Ordering::Acquire);
            if generation == 0 {
                return None;
            }
            let (segment, anchor) = self.seq.read(|| {
                (
                    self.segment.load(Ordering::Relaxed),
                    self.anchor.load(Ordering::Relaxed),
                )
            });
            // The body is coherent with `generation` iff `active` is still `generation` after
            // the snapshot. Generations are monotonic, so there is no ABA.
            if self.active.load(Ordering::Acquire) == generation {
                return Some(AnchorEntry {
                    segment,
                    anchor,
                    generation,
                });
            }
            spin_loop();
        }
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

    /// On-core single-writer publish. Hides the demand (`active = 0`) for the
    /// duration of the body write so a racing off-RT reader never pairs a fresh
    /// generation with a half-written body.
    pub(super) fn set(&self, segment: u32, anchor: u64) {
        let generation = self.next_gen();
        self.active.store(0, Ordering::Release);
        self.seq.begin();
        self.segment.store(segment, Ordering::Relaxed);
        self.anchor.store(anchor, Ordering::Relaxed);
        self.seq.end();
        self.active.store(generation, Ordering::Release);
    }
}

const NONE_ANCHOR: u64 = u64::MAX;

/// Lock-free, allocation-free seek-alias snapshot. The base `{segment, anchor}`
/// is a single-writer [`SeqAnchorCell`] (on-core SET/CLEAR); `exact_anchor` is
/// resolved off-RT and tagged with the base generation it belongs to, so a
/// stale resolver can never attach an exact anchor to a newer alias base.
pub(super) struct AtomicSeekAlias {
    /// Resolved exact anchor (`u64::MAX` = none).
    exact_anchor: AtomicU64,
    /// Base generation `exact_anchor` belongs to (0 = none).
    exact_gen: AtomicU64,
    base: SeqAnchorCell,
}

#[derive(Clone, Copy)]
pub(super) struct AliasSnapshot {
    pub(super) exact_anchor: Option<u64>,
    pub(super) segment: u32,
    pub(super) anchor: u64,
}

impl AliasSnapshot {
    pub(super) fn covers_position(self, pos: u64) -> bool {
        pos == self.anchor || self.exact_anchor == Some(pos)
    }
}

impl AtomicSeekAlias {
    pub(super) const fn new() -> Self {
        Self {
            base: SeqAnchorCell::new(),
            exact_anchor: AtomicU64::new(NONE_ANCHOR),
            exact_gen: AtomicU64::new(0),
        }
    }

    pub(super) fn clear(&self) {
        self.base.clear();
        self.exact_gen.store(0, Ordering::Release);
        self.exact_anchor.store(NONE_ANCHOR, Ordering::Relaxed);
    }

    pub(super) fn load(&self) -> Option<AliasSnapshot> {
        let base = self.base.load()?;
        // Accept `exact_anchor` only when its tag matches the live base
        // generation; a stale resolver leaves a mismatching tag we ignore.
        let exact_anchor = if self.exact_gen.load(Ordering::Acquire) == base.generation {
            match self.exact_anchor.load(Ordering::Relaxed) {
                NONE_ANCHOR => None,
                value => Some(value),
            }
        } else {
            None
        };
        Some(AliasSnapshot {
            exact_anchor,
            segment: base.segment,
            anchor: base.anchor,
        })
    }

    /// Off-RT: attach a resolved exact anchor to the matching base demand. A
    /// no-op if the base no longer matches; the generation tag makes a stale
    /// store harmless (the reader rejects a mismatching tag).
    pub(super) fn resolve(&self, segment: u32, anchor: u64, exact_anchor: u64) {
        let Some(base) = self.base.load() else {
            return;
        };
        if base.segment != segment || base.anchor != anchor {
            return;
        }
        self.exact_anchor.store(exact_anchor, Ordering::Relaxed);
        self.exact_gen.store(base.generation, Ordering::Release);
    }

    /// On-core single-writer publish of a fresh base; clears any prior exact
    /// anchor before the new generation goes live.
    pub(super) fn set(&self, anchor: u64, segment: u32) {
        self.exact_gen.store(0, Ordering::Release);
        self.exact_anchor.store(NONE_ANCHOR, Ordering::Relaxed);
        self.base.set(segment, anchor);
    }
}

/// `Option<u64>` packed into a single atomic, `u64::MAX` reserved for `None`
/// (a `2^64 - 1` byte offset is unreachable). Multi-writer-safe and
/// allocation-free on both read and clear.
pub(super) struct AtomicOptU64 {
    value: AtomicU64,
}

impl AtomicOptU64 {
    pub(super) fn load(&self) -> Option<u64> {
        match self.value.load(Ordering::Acquire) {
            u64::MAX => None,
            value => Some(value),
        }
    }

    pub(super) const fn none() -> Self {
        Self {
            value: AtomicU64::new(u64::MAX),
        }
    }

    pub(super) fn store(&self, value: Option<u64>) {
        self.value
            .store(value.unwrap_or(u64::MAX), Ordering::Release);
    }
}
