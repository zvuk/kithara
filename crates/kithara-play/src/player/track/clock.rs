use kithara_command::{Seq, When};
use kithara_render::bridge::{DeckPart, Slot, TrackClock};

use super::{super::outbox::Outbox, core::PlayerImpl};
use crate::PlayError;

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
        let Some(slot) = self.slot else {
            return Ok(None);
        };
        if self.clock == self.seated_clock || self.attaching.is_some() || self.attach_at.is_none() {
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
}
