use kithara_events::{AbrProgressSnapshot, VariantInfo};
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;

use crate::state::AbrState;

/// Protocol-agnostic interface the shared [`AbrController`](crate::AbrController)
/// uses to drive per-peer decisions.
///
/// `HlsPeer` provides the full set of capabilities; simpler peers (e.g. a
/// direct file download) rely on the default methods — no variants, no state,
/// no progress. The track-scoped event bus is owned by
/// [`AbrHandle`](crate::AbrHandle); peers do not need to juggle it.
#[kithara::mock(api = AbrMock)]
pub trait Abr: Send + Sync + 'static {
    /// Pull-model buffer observation used for buffer-aware decisions.
    /// Returning `None` disables buffer gates for this peer.
    fn progress(&self) -> Option<AbrProgressSnapshot> {
        None
    }

    /// Per-peer ABR state. Peers without variant switching return `None`.
    fn state(&self) -> Option<Arc<AbrState>> {
        None
    }

    /// All variants known to the peer.
    fn variants(&self) -> Vec<VariantInfo> {
        Vec::new()
    }

    /// Wake the peer's poll loop. Called by the controller after
    /// `request_target` writes a pending decision (e.g. on
    /// `AbrHandle::set_mode`) so the peer observes the pending intent
    /// without waiting for an unrelated event (seek, eviction, …).
    ///
    /// Without this hook the prod path "all variant segments cached
    /// → user clicks Manual(N)" silently drops the request: the peer
    /// is parked in `Poll::Pending` (nothing to fetch), `set_mode`
    /// updates the state slot, but the boundary-commit code in
    /// `apply_boundary_crossing` never runs and the variant never
    /// flips. The default `no-op` is correct for peers without
    /// variants — `HlsPeer` overrides it to wake `reader_advanced`.
    fn wake(&self) {}
}
