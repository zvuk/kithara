use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
};

use crate::{
    segment::fetch::{FetchClaim, PlannedFetch},
    variant::HlsVariant,
};

/// Lock-free four-valued cache-state discriminant for a segment / init
/// slot. `Downloading` exists to dedupe in-flight fetches: `dispatch`
/// only claims (`Missing -> Downloading`) slots before emitting a
/// `FetchCmd`. The settle path drives `Downloading -> Loaded` (success or
/// "another writer already committed"), `Downloading -> Missing`
/// (recoverable failure / cancel), and `Downloading -> Failed` (terminal:
/// the downloader exhausted its retry budget). Eviction is the only
/// producer of `Loaded -> Missing`.
///
/// `Failed` is terminal by construction: `try_claim` only CAS's from
/// `Missing`, so a failed slot is never re-dispatched (no extra scheduler
/// check needed) and a reader observing it via `is_failed` surfaces a
/// terminal error instead of spinning.
///
/// The bit values are private and the only mutators are the typed
/// transitions on the phase-specific `impl FetchClaim<Downloading>` /
/// `impl FetchClaim<Loaded>` blocks, so there is no silent `From<u8>`
/// fallback. Reads stay a plain atomic (no lock) because `download_head`
/// scans every slot on the ABR tick.
#[derive(Debug)]
pub(crate) struct SegmentSlotState(AtomicU8);

impl SegmentSlotState {
    const DOWNLOADING: u8 = 1;
    const FAILED: u8 = 3;
    const LOADED: u8 = 2;
    const MISSING: u8 = 0;

    pub(crate) fn is_downloading(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::DOWNLOADING
    }

    /// Terminal-failure probe. A `Failed` slot will never load (the
    /// downloader gave up); readers surface a terminal error on it.
    pub(crate) fn is_failed(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::FAILED
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::LOADED
    }

    pub(crate) fn mark_failed(&self) {
        self.0.store(Self::FAILED, Ordering::Release);
    }

    pub(crate) fn mark_loaded(&self) {
        self.0.store(Self::LOADED, Ordering::Release);
    }

    pub(crate) fn mark_missing(&self) {
        self.0.store(Self::MISSING, Ordering::Release);
    }

    pub(crate) fn missing() -> Arc<Self> {
        Arc::new(Self(AtomicU8::new(Self::MISSING)))
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
                Self::MISSING,
                Self::DOWNLOADING,
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
