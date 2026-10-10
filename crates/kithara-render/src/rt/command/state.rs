use kithara_command::Seq;
use kithara_signal::SegmentId;

use super::super::track::PlayerResource;
use crate::bridge::{DeckPart, Returned, Slot, SlotMark};

#[derive(Clone, Copy)]
pub(in crate::rt) enum EndAction {
    Chain { to: Slot },
    Adopt { segment: SegmentId },
}

#[derive(Clone, Copy)]
pub(in crate::rt) struct Armed {
    pub(in crate::rt) seq: Seq,
    pub(in crate::rt) action: EndAction,
}

pub(in crate::rt) fn shifted_slot(part: &DeckPart) -> Option<Slot> {
    match part {
        DeckPart::Attach { slot, .. }
        | DeckPart::Detach { slot }
        | DeckPart::Start { slot, .. }
        | DeckPart::Stop { slot, .. }
        | DeckPart::Adopt { slot, .. }
        | DeckPart::Fade { slot, .. }
        | DeckPart::Replace { slot, .. } => Some(*slot),
        DeckPart::Chain { .. } | DeckPart::Mix(_) | DeckPart::Eq(_) | DeckPart::Returned(_) => None,
    }
}

pub(in crate::rt) fn replace_stop(commands: &mut [DeckPart], slot: Slot, resume: SlotMark) {
    if let Some(part) = commands
        .iter_mut()
        .rev()
        .find(|part| matches!(part, DeckPart::Stop { slot: target, .. } if *target == slot))
    {
        *part = DeckPart::Returned(Returned::Stopped { slot, resume });
    }
}

pub(in crate::rt) fn stops_complete(commands: &[DeckPart]) -> bool {
    !commands
        .iter()
        .any(|part| matches!(part, DeckPart::Stop { .. }))
}

pub(in crate::rt) fn returned_pcm(slot: Slot, pcm: Box<PlayerResource>) -> DeckPart {
    DeckPart::Returned(Returned::Pcm { slot, pcm })
}
