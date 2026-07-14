use kithara_platform::sync::Arc;

use crate::{impls::resource::Resource, types::SlotId};

/// A queued resource plus its optional queue-item identity.
pub(crate) struct QueuedResource {
    pub(crate) item_id: Option<Arc<str>>,
    pub(crate) resource: Resource,
}

/// Whether the armed successor has been activated for the current handover.
///
/// Mirrors the pre-split `PendingNext::activated: bool`:
/// - `Armed` ⇒ `activated == false` (armed, not yet committed).
/// - `ActivatedReady` ⇒ `activated == true` (committed, leading slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingNextState {
    Armed,
    ActivatedReady,
}

impl PendingNextState {
    pub(crate) fn activated(self) -> bool {
        matches!(self, Self::ActivatedReady)
    }
}

/// Internal auto-advance state for the next queue item.
///
/// The queue still owns `current_index`; `PendingNext` only tracks the
/// already-enqueued successor and whether it has been activated.
pub(crate) struct PendingNext {
    pub(crate) src: Arc<str>,
    pub(crate) state: PendingNextState,
    pub(crate) duration_seconds: f64,
    pub(crate) index: usize,
}

/// Discriminant for [`PlayerPhase`] without its payload.
///
/// `#[non_exhaustive]` so adding a variant later is not a breaking change for
/// in-crate match sites that already handle the wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum PlayerPhaseKind {
    Idle,
    Loading,
    Playing,
    Paused,
    Stopped,
}

/// Active phase of the player, folding the previously-independent
/// `current_slot` / `current_abr_handle` / `pending_next` mutexes into a
/// single typed phase guarded by one `Mutex<PlayerPhase>`.
///
/// A slot, once allocated, is reused for the player's lifetime: both
/// `Playing` and `Paused` carry the same `slot`. `Loading` is the transient
/// state between slot allocation and the first track being driven; `Idle` is
/// the freshly-constructed state with no slot; `Stopped` follows
/// `remove_all_items` (cleared queue, slot may still exist).
pub(crate) enum PlayerPhase {
    Idle,
    Loading {
        slot: SlotId,
        abr_handle: Option<kithara_abr::AbrHandle>,
        pending: Option<PendingNext>,
    },
    Playing {
        slot: SlotId,
        abr_handle: Option<kithara_abr::AbrHandle>,
        pending: Option<PendingNext>,
    },
    Paused {
        slot: SlotId,
        abr_handle: Option<kithara_abr::AbrHandle>,
        pending: Option<PendingNext>,
    },
    Stopped {
        slot: Option<SlotId>,
        abr_handle: Option<kithara_abr::AbrHandle>,
    },
}

impl From<&PlayerPhase> for PlayerPhaseKind {
    fn from(phase: &PlayerPhase) -> Self {
        match phase {
            PlayerPhase::Idle => Self::Idle,
            PlayerPhase::Loading { .. } => Self::Loading,
            PlayerPhase::Playing { .. } => Self::Playing,
            PlayerPhase::Paused { .. } => Self::Paused,
            PlayerPhase::Stopped { .. } => Self::Stopped,
        }
    }
}

impl PlayerPhase {
    delegate::delegate! {
        to self {
            /// The ABR handle of the resource currently in the processor, if any.
            #[expr($.cloned())]
            #[call(abr_handle_ref)]
            pub(crate) fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            #[call(into)]
            pub(crate) fn kind(&self) -> PlayerPhaseKind;
            /// The active slot, if any phase currently holds one.
            #[expr($.copied())]
            #[call(slot_ref)]
            pub(crate) fn slot(&self) -> Option<SlotId>;
        }
    }

    /// Borrow of the active ABR handle slot. Returning a reference makes this
    /// a true accessor (not a `self -> Other` conversion).
    fn abr_handle_ref(&self) -> Option<&kithara_abr::AbrHandle> {
        match self {
            Self::Idle => None,
            Self::Loading { abr_handle, .. }
            | Self::Playing { abr_handle, .. }
            | Self::Paused { abr_handle, .. }
            | Self::Stopped { abr_handle, .. } => abr_handle.as_ref(),
        }
    }

    /// Shared read access to the armed-next slot, if any.
    pub(crate) fn pending(&self) -> Option<&PendingNext> {
        match self {
            Self::Loading { pending, .. }
            | Self::Playing { pending, .. }
            | Self::Paused { pending, .. } => pending.as_ref(),
            Self::Idle | Self::Stopped { .. } => None,
        }
    }

    /// Mutable access to the armed-next slot for transition bookkeeping.
    pub(crate) fn pending_mut(&mut self) -> Option<&mut Option<PendingNext>> {
        match self {
            Self::Loading { pending, .. }
            | Self::Playing { pending, .. }
            | Self::Paused { pending, .. } => Some(pending),
            Self::Idle | Self::Stopped { .. } => None,
        }
    }

    /// Replace the ABR handle on the active phase (no-op from `Idle`).
    pub(crate) fn set_abr_handle(&mut self, handle: Option<kithara_abr::AbrHandle>) {
        match self {
            Self::Loading { abr_handle, .. }
            | Self::Playing { abr_handle, .. }
            | Self::Paused { abr_handle, .. }
            | Self::Stopped { abr_handle, .. } => *abr_handle = handle,
            Self::Idle => {}
        }
    }

    /// Borrow of the active slot. Returning a reference makes this a true
    /// accessor (not a `self -> Other` conversion).
    fn slot_ref(&self) -> Option<&SlotId> {
        match self {
            Self::Idle => None,
            Self::Loading { slot, .. } | Self::Playing { slot, .. } | Self::Paused { slot, .. } => {
                Some(slot)
            }
            Self::Stopped { slot, .. } => slot.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn pending_next_state_maps_activated_bool() {
        // `Armed` mirrors the pre-split `activated == false`,
        // `ActivatedReady` mirrors `activated == true`.
        assert!(!PendingNextState::Armed.activated());
        assert!(PendingNextState::ActivatedReady.activated());
    }

    #[kithara::test]
    fn player_phase_kind_exhaustive() {
        assert_eq!(PlayerPhase::Idle.kind(), PlayerPhaseKind::Idle);
        let slot = SlotId::new(0);
        assert_eq!(
            PlayerPhase::Loading {
                slot,
                abr_handle: None,
                pending: None,
            }
            .kind(),
            PlayerPhaseKind::Loading
        );
        assert_eq!(
            PlayerPhase::Playing {
                slot,
                abr_handle: None,
                pending: None,
            }
            .kind(),
            PlayerPhaseKind::Playing
        );
        assert_eq!(
            PlayerPhase::Paused {
                slot,
                abr_handle: None,
                pending: None,
            }
            .kind(),
            PlayerPhaseKind::Paused
        );
        assert_eq!(
            PlayerPhase::Stopped {
                slot: None,
                abr_handle: None,
            }
            .kind(),
            PlayerPhaseKind::Stopped
        );
    }

    #[kithara::test]
    fn idle_phase_has_no_slot_or_handle() {
        let phase = PlayerPhase::Idle;
        assert!(phase.slot().is_none());
        assert!(phase.abr_handle().is_none());
        assert!(phase.pending().is_none());
    }
}
