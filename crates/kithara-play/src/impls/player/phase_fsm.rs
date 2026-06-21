use super::{
    core::PlayerImpl,
    error::TransitionError,
    phase::{PlayerPhase, PlayerPhaseKind},
};
use crate::{error::PlayError, impls::player_processor::PlayerCmd, types::SlotId};

impl PlayerImpl {
    /// Promote the phase to `Loading` carrying `slot`, preserving any armed
    /// next / ABR handle the previous active phase held. A no-op transition
    /// when the phase already holds a slot keeps the existing payload.
    pub(crate) fn enter_loading_with_slot(&self, slot: SlotId) {
        let mut phase = self.phase.lock();
        let (abr_handle, pending) = match std::mem::replace(&mut *phase, PlayerPhase::Idle) {
            PlayerPhase::Loading {
                abr_handle,
                pending,
                ..
            }
            | PlayerPhase::Playing {
                abr_handle,
                pending,
                ..
            }
            | PlayerPhase::Paused {
                abr_handle,
                pending,
                ..
            } => (abr_handle, pending),
            PlayerPhase::Stopped { abr_handle, .. } => (abr_handle, None),
            PlayerPhase::Idle => (None, None),
        };
        *phase = PlayerPhase::Loading {
            slot,
            abr_handle,
            pending,
        };
    }

    /// Move an active (slot-holding) phase into `Paused`. No-op from `Idle`.
    pub(crate) fn enter_paused(&self) {
        let mut phase = self.phase.lock();
        match std::mem::replace(&mut *phase, PlayerPhase::Idle) {
            PlayerPhase::Loading {
                slot,
                abr_handle,
                pending,
            }
            | PlayerPhase::Playing {
                slot,
                abr_handle,
                pending,
            }
            | PlayerPhase::Paused {
                slot,
                abr_handle,
                pending,
            } => {
                *phase = PlayerPhase::Paused {
                    slot,
                    abr_handle,
                    pending,
                };
            }
            PlayerPhase::Stopped { slot, abr_handle } => {
                *phase = match slot {
                    Some(slot) => PlayerPhase::Paused {
                        slot,
                        abr_handle,
                        pending: None,
                    },
                    None => PlayerPhase::Stopped { slot, abr_handle },
                };
            }
            PlayerPhase::Idle => *phase = PlayerPhase::Idle,
        }
    }

    /// Move an active (slot-holding) phase into `Playing`. No-op from `Idle`.
    pub(crate) fn enter_playing(&self) {
        let mut phase = self.phase.lock();
        match std::mem::replace(&mut *phase, PlayerPhase::Idle) {
            PlayerPhase::Loading {
                slot,
                abr_handle,
                pending,
            }
            | PlayerPhase::Playing {
                slot,
                abr_handle,
                pending,
            }
            | PlayerPhase::Paused {
                slot,
                abr_handle,
                pending,
            } => {
                *phase = PlayerPhase::Playing {
                    slot,
                    abr_handle,
                    pending,
                };
            }
            PlayerPhase::Stopped { slot, abr_handle } => {
                *phase = match slot {
                    Some(slot) => PlayerPhase::Playing {
                        slot,
                        abr_handle,
                        pending: None,
                    },
                    None => PlayerPhase::Stopped { slot, abr_handle },
                };
            }
            PlayerPhase::Idle => *phase = PlayerPhase::Idle,
        }
    }

    /// Move the player into `Stopped`, preserving the slot/ABR handle.
    pub(crate) fn enter_stopped(&self) {
        let mut phase = self.phase.lock();
        let (slot, abr_handle) = match std::mem::replace(&mut *phase, PlayerPhase::Idle) {
            PlayerPhase::Loading {
                slot, abr_handle, ..
            }
            | PlayerPhase::Playing {
                slot, abr_handle, ..
            }
            | PlayerPhase::Paused {
                slot, abr_handle, ..
            } => (Some(slot), abr_handle),
            PlayerPhase::Stopped { slot, abr_handle } => (slot, abr_handle),
            PlayerPhase::Idle => (None, None),
        };
        *phase = PlayerPhase::Stopped { slot, abr_handle };
    }

    /// Discriminant of the current phase under a short lock.
    pub(crate) fn phase_kind(&self) -> PlayerPhaseKind {
        self.phase.lock().kind()
    }

    /// Phase gate: the active slot, or [`TransitionError::WrongPhase`] when
    /// the player holds no slot (phases `Idle` / `Stopped`-without-slot).
    /// This is the single internal phase predicate; public wrappers absorb
    /// `WrongPhase` to preserve the pre-split no-op / typed-error contracts.
    pub(crate) fn require_active_slot(&self) -> Result<SlotId, TransitionError> {
        self.phase.lock().slot().ok_or(TransitionError::WrongPhase)
    }

    /// Send a command to the current slot's processor.
    ///
    /// Absorbs the internal [`TransitionError::WrongPhase`] (no slot) into the
    /// pre-split `PlayError::Internal("no active slot")` so the silent-no-op
    /// wrappers (`pause`, `set_rate`, `set_crossfade_duration`, …) keep their
    /// `let _ = self.send_to_slot(..)` behaviour from `Idle`/`Stopped`.
    pub(crate) fn send_to_slot(&self, cmd: PlayerCmd) -> Result<(), PlayError> {
        let slot_id = self
            .require_active_slot()
            .map_err(|TransitionError::WrongPhase| PlayError::Internal("no active slot".into()))?;
        self.core.engine.send_slot_cmd(slot_id, cmd)
    }

    /// Snapshot of the active slot under a short phase lock. Absorbs
    /// `WrongPhase` into `None` for query/no-op call sites.
    pub(crate) fn slot(&self) -> Option<SlotId> {
        self.require_active_slot().ok()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::impls::player::PlayerConfig;

    #[kithara::test]
    fn require_active_slot_errors_from_idle() {
        let player = PlayerImpl::new(PlayerConfig::default());
        assert_eq!(
            player.require_active_slot(),
            Err(TransitionError::WrongPhase)
        );
    }
}
