use kithara_events::{SyncEvent, TransportEvent};

use super::{
    convert::NotForwarded,
    types::{FfiPlaybackDirection, FfiPlayerEvent},
};

impl TryFrom<&TransportEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &TransportEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            TransportEvent::TempoCommitted {
                beats_per_minute,
                revision,
            } => Self::TransportTempoCommitted {
                beats_per_minute: *beats_per_minute,
                revision: *revision,
            },
            TransportEvent::PlayStateCommitted { playing, revision } => {
                Self::TransportPlayStateCommitted {
                    playing: *playing,
                    revision: *revision,
                }
            }
            TransportEvent::SeekCommitted {
                position_beats,
                revision,
            } => Self::TransportSeekCommitted {
                position_beats: *position_beats,
                revision: *revision,
            },
            TransportEvent::Failed { revision, reason } => Self::TransportFailed {
                revision: *revision,
                reason: reason.clone(),
            },
            _ => return Err(NotForwarded),
        })
    }
}

impl TryFrom<&SyncEvent> for FfiPlayerEvent {
    type Error = NotForwarded;

    fn try_from(event: &SyncEvent) -> Result<Self, NotForwarded> {
        Ok(match event {
            SyncEvent::BindingCommitted {
                slot,
                session_anchor_beats,
                track_anchor_beats,
                direction,
            } => Self::SyncBindingCommitted {
                slot: slot.value(),
                session_anchor_beats: *session_anchor_beats,
                track_anchor_beats: *track_anchor_beats,
                direction: FfiPlaybackDirection::from(*direction),
            },
            SyncEvent::LockAcquired { slot, revision } => Self::SyncLockAcquired {
                slot: slot.value(),
                revision: *revision,
            },
            SyncEvent::LockLost { slot, revision } => Self::SyncLockLost {
                slot: slot.value(),
                revision: *revision,
            },
            SyncEvent::RelockCommitted {
                slot,
                position_beats,
                revision,
            } => Self::SyncRelockCommitted {
                slot: slot.value(),
                position_beats: *position_beats,
                revision: *revision,
            },
            SyncEvent::DirectionCommitted {
                slot,
                direction,
                revision,
            } => Self::SyncDirectionCommitted {
                slot: slot.value(),
                direction: FfiPlaybackDirection::from(*direction),
                revision: *revision,
            },
            SyncEvent::Unavailable { slot, reason } => Self::SyncUnavailable {
                slot: slot.value(),
                reason: reason.clone(),
            },
            _ => return Err(NotForwarded),
        })
    }
}
