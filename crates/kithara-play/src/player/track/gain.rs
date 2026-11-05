use kithara_command::{Seq, Target, When};
use kithara_render::bridge::{DeckPart, SlotGainProgram, SlotState};

use super::{core::PlayerImpl, state::TrackStatus};
use crate::{Outbox, PlayError};

impl<S> PlayerImpl<S> {
    pub(super) fn gain_program(
        &mut self,
        program: SlotGainProgram,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if self.releasing
            || self.preparation_closed()
            || !self.attached()
            || self.ready != Some(self.segment)
            || matches!(
                self.status,
                TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Ended { .. }
            )
        {
            return Err(PlayError::NotReady);
        }
        let at = When::At(program.start());
        self.check_when(at, out)?;
        let slot = self.seat()?;
        let observed = out
            .pass()
            .and_then(|pass| pass.deck.slots.get(slot.index()))
            .ok_or(PlayError::NotReady)?;
        let binding = observed
            .binding
            .filter(|binding| binding.item() == self.item && binding.segment() == self.segment)
            .filter(|_| observed.state != SlotState::Ended)
            .ok_or(PlayError::NotReady)?;
        self.send_playback(
            at,
            vec![DeckPart::GainProgram {
                slot,
                binding,
                authority: observed.output.authority(),
                program,
            }],
            out,
        )
    }
}
