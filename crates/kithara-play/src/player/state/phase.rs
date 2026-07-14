use kithara_platform::sync::Arc;

use super::super::core::PlayerImpl;
use crate::{
    api::{PlayerEvent, SlotId, TimeControlStatus, WaitingReason},
    bridge::PlayerCmd,
    error::PlayError,
};

/// Internal phase-transition error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionError {
    /// The requested action is not valid from the current phase.
    WrongPhase,
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
/// `Playlist` owns the current index; `PendingNext` only tracks the
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

    pub(crate) fn enter_loading_with_slot(&mut self, slot: SlotId) {
        let (abr_handle, pending) = match std::mem::replace(self, Self::Idle) {
            Self::Loading {
                abr_handle,
                pending,
                ..
            }
            | Self::Playing {
                abr_handle,
                pending,
                ..
            }
            | Self::Paused {
                abr_handle,
                pending,
                ..
            } => (abr_handle, pending),
            Self::Stopped { abr_handle, .. } => (abr_handle, None),
            Self::Idle => (None, None),
        };
        *self = Self::Loading {
            slot,
            abr_handle,
            pending,
        };
    }

    pub(crate) fn enter_paused(&mut self) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Loading {
                slot,
                abr_handle,
                pending,
            }
            | Self::Playing {
                slot,
                abr_handle,
                pending,
            }
            | Self::Paused {
                slot,
                abr_handle,
                pending,
            } => Self::Paused {
                slot,
                abr_handle,
                pending,
            },
            Self::Stopped {
                slot: Some(slot),
                abr_handle,
            } => Self::Paused {
                slot,
                abr_handle,
                pending: None,
            },
            phase => phase,
        };
    }

    pub(crate) fn enter_playing(&mut self) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Loading {
                slot,
                abr_handle,
                pending,
            }
            | Self::Playing {
                slot,
                abr_handle,
                pending,
            }
            | Self::Paused {
                slot,
                abr_handle,
                pending,
            } => Self::Playing {
                slot,
                abr_handle,
                pending,
            },
            Self::Stopped {
                slot: Some(slot),
                abr_handle,
            } => Self::Playing {
                slot,
                abr_handle,
                pending: None,
            },
            phase => phase,
        };
    }

    pub(crate) fn enter_stopped(&mut self) {
        let (slot, abr_handle) = match std::mem::replace(self, Self::Idle) {
            Self::Loading {
                slot, abr_handle, ..
            }
            | Self::Playing {
                slot, abr_handle, ..
            }
            | Self::Paused {
                slot, abr_handle, ..
            } => (Some(slot), abr_handle),
            Self::Stopped { slot, abr_handle } => (slot, abr_handle),
            Self::Idle => (None, None),
        };
        *self = Self::Stopped { slot, abr_handle };
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

impl PlayerImpl {
    fn publish_time_control_status(
        &self,
        status: TimeControlStatus,
        reason: Option<WaitingReason>,
    ) {
        self.core
            .engine
            .bus()
            .publish(PlayerEvent::TimeControlStatusChanged { status, reason });
    }

    delegate::delegate! {
        to self.phase.lock() {
            /// Discriminant of the current phase under a short lock.
            #[call(kind)]
            pub(crate) fn phase_kind(&self) -> PlayerPhaseKind;
            /// Phase gate: the active slot, or [`TransitionError::WrongPhase`] when
            /// the player holds no slot (phases `Idle` / `Stopped`-without-slot).
            #[expr($.ok_or(TransitionError::WrongPhase))]
            #[call(slot)]
            pub(crate) fn require_active_slot(&self) -> Result<SlotId, TransitionError>;
        }
    }

    /// Promote the phase to `Loading` carrying `slot`, preserving any armed
    /// next / ABR handle the previous active phase held. A no-op transition
    /// when the phase already holds a slot keeps the existing payload.
    pub(crate) fn enter_loading_with_slot(&self, slot: SlotId) {
        self.phase.lock().enter_loading_with_slot(slot);
        self.publish_time_control_status(TimeControlStatus::WaitingToPlay, None);
    }

    /// Move an active (slot-holding) phase into `Paused`. No-op from `Idle`.
    pub(crate) fn enter_paused(&self) {
        self.phase.lock().enter_paused();
        self.publish_time_control_status(TimeControlStatus::Paused, None);
    }

    /// Move an active (slot-holding) phase into `Playing`. No-op from `Idle`.
    pub(crate) fn enter_playing(&self) {
        self.phase.lock().enter_playing();
        self.publish_time_control_status(TimeControlStatus::Playing, None);
    }

    /// Move the player into `Stopped`, preserving the slot/ABR handle.
    pub(crate) fn enter_stopped(&self) {
        self.phase.lock().enter_stopped();
        self.publish_time_control_status(TimeControlStatus::Paused, None);
    }

    /// Send a command to the current slot's processor.
    pub(crate) fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = self
            .require_active_slot()
            .map_err(|TransitionError::WrongPhase| PlayError::NoActiveSlot)?;
        self.core.engine.send_slot_cmd(slot_id, cmd)
    }

    /// Snapshot of the active slot under a short phase lock.
    pub(crate) fn slot(&self) -> Option<SlotId> {
        self.require_active_slot().ok()
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

    #[kithara::test]
    fn require_active_slot_errors_from_idle() {
        let player = PlayerImpl::new(crate::player::PlayerConfig::default());
        assert_eq!(
            player.require_active_slot(),
            Err(TransitionError::WrongPhase)
        );
    }
}
