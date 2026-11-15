//! Deck parts a track sends its seated RT track outside transport: the clock
//! it keeps and its gain programs.

use kithara_command::{Seq, Target, When};
use kithara_render::bridge::{DeckPart, Slot, SlotGainProgram, SlotState, TrackClock};

use super::{core::PlayerImpl, state::TrackStatus};
use crate::{Outbox, PlayError};

impl<S> PlayerImpl<S> {
    /// Seat parts carry the clock a fresh RT track lacks.
    pub(super) fn seated_with_clock(
        &mut self,
        slot: Slot,
        mut parts: Vec<DeckPart>,
    ) -> Vec<DeckPart> {
        if self.clock != TrackClock::Media {
            parts.push(DeckPart::Clock {
                slot,
                clock: self.clock,
            });
        }
        self.seated_clock = self.clock;
        parts
    }

    /// Sends a changed clock to the RT track the deck has already seated; a
    /// seat still in flight carries it itself.
    pub(super) fn sync_clock(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        let Some(slot) = self.seat.slot() else {
            return Ok(None);
        };
        if self.clock == self.seated_clock || self.seat.attached_at().is_none() {
            return Ok(None);
        }
        let sent = out.deck(
            When::Next,
            vec![DeckPart::Clock {
                slot,
                clock: self.clock,
            }],
        )?;
        self.seated_clock = self.clock;
        Ok(sent)
    }

    pub(super) fn gain_program(
        &mut self,
        program: SlotGainProgram,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if self.seat.releasing()
            || self.preparation_closed()
            || !self.attached()
            || !self.segment.ready
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
            .filter(|binding| binding.item() == self.item && binding.segment() == self.segment.id)
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
