use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_play::{DeckProgram, SlotGainProgram};

use super::{QueueCommand, QueueControl};
use crate::QueueError;

impl<S> QueueControl<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Submit a complete-output amplitude program for one prepared queue item.
    /// # Errors
    /// Refuses unavailable/full control delivery; the owner checks source identity and lead.
    pub fn gain_program(&self, item: TrackId, program: SlotGainProgram) -> Result<(), QueueError> {
        self.call(QueueCommand::GainProgram { item, program })
    }

    /// Submit a prepared gain trajectory for one control of this deck.
    /// # Errors
    /// Refuses unavailable/full control delivery; the owner checks layout and clock identity.
    pub fn deck_program(&self, program: DeckProgram) -> Result<(), QueueError> {
        self.deck_programs(vec![program])
    }
    /// Submit programs together; a refusal changes none of the addressed controls.
    /// # Errors
    /// Returns preparation or delivery refusal from the deck owner.
    pub fn deck_programs(&self, programs: Vec<DeckProgram>) -> Result<(), QueueError> {
        self.call(QueueCommand::DeckPrograms(programs))
    }
}
