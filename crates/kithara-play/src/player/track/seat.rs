use kithara_command::{Seq, When};
use kithara_render::bridge::Slot;
use kithara_signal::SessionFrame;

#[derive(Clone, Copy)]
pub(super) struct Attaching {
    pub(super) seq: Option<Seq>,
    pub(super) caller: Seq,
    pub(super) at: When<SessionFrame>,
    pub(super) replacement: bool,
    pub(super) play: Option<When<SessionFrame>>,
    pub(super) obsolete: bool,
}

/// Where the track stands on the deck.
pub(super) enum Seat {
    /// Off the deck: not loaded, or loaded in the background.
    Unseated,
    /// The slot is named and the deck holds no consumer of this track in it.
    Named(Slot),
    /// The deck batch that puts the consumer in the slot is in flight.
    Attaching {
        slot: Slot,
        attach: Attaching,
        releasing: bool,
    },
    /// The deck holds the consumer since `at`.
    Attached {
        slot: Slot,
        seq: Seq,
        at: SessionFrame,
        releasing: bool,
    },
}

impl Seat {
    pub(super) fn slot(&self) -> Option<Slot> {
        match *self {
            Self::Unseated => None,
            Self::Named(slot) | Self::Attaching { slot, .. } | Self::Attached { slot, .. } => {
                Some(slot)
            }
        }
    }

    pub(super) fn attaching(&self) -> Option<&Attaching> {
        match self {
            Self::Attaching { attach, .. } => Some(attach),
            _ => None,
        }
    }

    pub(super) fn attaching_mut(&mut self) -> Option<&mut Attaching> {
        match self {
            Self::Attaching { attach, .. } => Some(attach),
            _ => None,
        }
    }

    pub(super) fn attached_at(&self) -> Option<SessionFrame> {
        match *self {
            Self::Attached { at, .. } => Some(at),
            _ => None,
        }
    }

    pub(super) fn attachment(&self) -> Option<Seq> {
        match self {
            Self::Attached { seq, .. } => Some(*seq),
            _ => None,
        }
    }

    /// A release asked for while a replacement was in flight: the detach goes
    /// out once the replacement lands.
    pub(super) fn releasing(&self) -> bool {
        matches!(
            self,
            Self::Attaching {
                releasing: true,
                ..
            } | Self::Attached {
                releasing: true,
                ..
            }
        )
    }

    pub(super) fn release_on_landing(&mut self) {
        if let Self::Attaching { releasing, .. } = self {
            *releasing = true;
        }
    }

    /// The attach in flight was applied on `at`.
    pub(super) fn land(&mut self, seq: Seq, at: SessionFrame) {
        if let Self::Attaching {
            slot, releasing, ..
        } = *self
        {
            *self = Self::Attached {
                slot,
                seq,
                at,
                releasing,
            };
        }
    }

    /// The deck let the consumer go; an attach still in flight stays.
    pub(super) fn detached(&mut self) {
        match self {
            Self::Attached { slot, .. } => *self = Self::Named(*slot),
            Self::Attaching { releasing, .. } => *releasing = false,
            Self::Unseated | Self::Named(_) => {}
        }
    }

    /// Nothing of this track is on the deck or on its way there.
    pub(super) fn vacate(&mut self) {
        if let Some(slot) = self.slot() {
            *self = Self::Named(slot);
        }
    }
}
