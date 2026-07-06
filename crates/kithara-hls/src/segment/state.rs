use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
};

use bitflags::bitflags;

use crate::{
    segment::fetch::{FetchClaim, PlannedFetch},
    variant::HlsVariant,
};

bitflags! {
    /// Lock-free slot flags packed into one `AtomicU8`. The cache state is
    /// mutually exclusive — at most one of [`DOWNLOADING`](SlotFlags::DOWNLOADING),
    /// [`LOADED`](SlotFlags::LOADED), [`FAILED`](SlotFlags::FAILED) is set
    /// (none = `Missing`) because every transition `store`s a single state
    /// value. [`SLOW`](SlotFlags::SLOW) is an orthogonal flag OR-ed on top
    /// while the in-flight fetch outlasts the downloader's `soft_timeout`.
    /// A state `store` clears `SLOW` for free, so it is only ever observed
    /// alongside `DOWNLOADING`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SlotFlags: u8 {
        const DOWNLOADING = 1 << 0;
        const LOADED      = 1 << 1;
        const FAILED      = 1 << 2;
        const SLOW        = 1 << 3;
    }
}

/// Lock-free cache-state discriminant for a segment / init slot.
/// `Downloading` exists to dedupe in-flight fetches: `dispatch` only claims
/// (`Missing -> Downloading`) slots before emitting a `FetchCmd`. The settle
/// path drives `Downloading -> Loaded` (success or "another writer already
/// committed"), `Downloading -> Missing` (recoverable failure / cancel), and
/// `Downloading -> Failed` (terminal: the downloader exhausted its retry
/// budget). Eviction is the only producer of `Loaded -> Missing`.
///
/// `Failed` is terminal by construction: `try_claim` only CAS's from
/// `Missing`, so a failed slot is never re-dispatched (no extra scheduler
/// check needed) and a reader observing it via `is_failed` surfaces a
/// terminal error instead of spinning.
///
/// The only mutators are the typed transitions on the phase-specific
/// `impl FetchClaim<Downloading>` / `impl FetchClaim<Loaded>` blocks (plus
/// the `on_slow` hook), so there is no silent fallback. Reads stay a plain
/// atomic (no lock) because `download_head` scans every slot on the ABR tick.
#[derive(Debug)]
pub(crate) struct SegmentSlotState(AtomicU8);

impl SegmentSlotState {
    fn flags(&self) -> SlotFlags {
        SlotFlags::from_bits_truncate(self.0.load(Ordering::Acquire))
    }

    pub(crate) fn is_downloading(&self) -> bool {
        self.flags().contains(SlotFlags::DOWNLOADING)
    }

    /// Terminal-failure probe. A `Failed` slot will never load (the
    /// downloader gave up); readers surface a terminal error on it.
    pub(crate) fn is_failed(&self) -> bool {
        self.flags().contains(SlotFlags::FAILED)
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.flags().contains(SlotFlags::LOADED)
    }

    /// True while the current in-flight fetch has crossed `soft_timeout`
    /// without settling. Meaningful only together with [`Self::is_downloading`].
    pub(crate) fn is_slow(&self) -> bool {
        self.flags().contains(SlotFlags::SLOW)
    }

    pub(crate) fn mark_failed(&self) {
        self.0.store(SlotFlags::FAILED.bits(), Ordering::Release);
    }

    pub(crate) fn mark_loaded(&self) {
        self.0.store(SlotFlags::LOADED.bits(), Ordering::Release);
    }

    pub(crate) fn mark_missing(&self) {
        self.0.store(SlotFlags::empty().bits(), Ordering::Release);
    }

    /// Mark the in-flight fetch slow (the `on_slow` hook fired). Idempotent;
    /// the next state `store` (terminal transition or fresh claim) clears it.
    pub(crate) fn mark_slow(&self) {
        self.0.fetch_or(SlotFlags::SLOW.bits(), Ordering::AcqRel);
    }

    pub(crate) fn missing() -> Arc<Self> {
        Arc::new(Self(AtomicU8::new(SlotFlags::empty().bits())))
    }

    /// Atomic `Missing -> Downloading` claim. Returns the owned
    /// [`FetchClaim<Downloading>`](FetchClaim) handle when the caller now owns
    /// the in-flight slot, `None` when another caller already claimed it.
    pub(crate) fn try_claim(
        self: &Arc<Self>,
        planned: PlannedFetch,
        variant: Weak<HlsVariant>,
    ) -> Option<FetchClaim<Downloading>> {
        self.0
            .compare_exchange(
                SlotFlags::empty().bits(),
                SlotFlags::DOWNLOADING.bits(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| FetchClaim::claim(planned, variant, Arc::clone(self)))
    }
}

mod sealed {
    pub(crate) trait Sealed {}
}

/// Compile-time download phase of a segment / init slot. The phantom
/// parameter on [`FetchClaim`](crate::segment::FetchClaim) encodes which transitions are legal, so the
/// invariants that `SegmentSlotState` used to check at runtime become
/// type errors: only a `FetchClaim<Downloading>` can settle, and it settles
/// by consuming itself into a `FetchClaim<Loaded>` or `FetchClaim<Missing>`.
///
/// Sealed — the phase set is closed to this module. Each phase carries its
/// own [`Data`](SegmentPhase::Data) payload; phases without state use `()`.
pub(crate) trait SegmentPhase: sealed::Sealed {
    type Data;
}

/// In-flight: claimed via a `Missing -> Downloading` CAS, fetch pending.
pub(crate) struct Downloading;
/// Committed on disk; carries the resolved `final_len`.
pub(crate) struct Loaded;
/// Returned to the dispatch pool (recoverable failure / cancel / evict).
pub(crate) struct Missing;
/// Terminal: the downloader exhausted its retry budget on this slot. Never
/// re-dispatched (`try_claim` only CAS's from `Missing`) and surfaced to
/// readers as a terminal error via [`SegmentSlotState::is_failed`].
pub(crate) struct Failed;

impl sealed::Sealed for Downloading {}
impl sealed::Sealed for Loaded {}
impl sealed::Sealed for Missing {}
impl sealed::Sealed for Failed {}

impl SegmentPhase for Downloading {
    type Data = super::fetch::DownloadClaim;
}
impl SegmentPhase for Loaded {
    type Data = super::fetch::LoadedProof;
}
impl SegmentPhase for Missing {
    type Data = ();
}
impl SegmentPhase for Failed {
    type Data = ();
}
