use std::sync::Arc;

use kithara_events::{AbrProgressSnapshot, AbrVariant};
#[cfg(any(test, feature = "internal"))]
use unimock::unimock;

use crate::state::AbrState;

/// Protocol-agnostic interface the shared [`AbrController`](crate::AbrController)
/// uses to drive per-peer decisions.
///
/// `HlsPeer` provides the full set of capabilities; simpler peers (e.g. a
/// direct file download) rely on the default methods — no variants, no state,
/// no progress. The track-scoped event bus is owned by
/// [`AbrHandle`](crate::AbrHandle); peers do not need to juggle it.
#[cfg_attr(any(test, feature = "internal"), unimock(api = AbrMock))]
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
    fn variants(&self) -> Vec<AbrVariant> {
        Vec::new()
    }
}
