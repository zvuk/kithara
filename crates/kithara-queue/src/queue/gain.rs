use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_play::{EqGainProgram, SlotGainProgram};

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

    /// Submit a prepared gain trajectory for one band of this deck's EQ.
    /// # Errors
    /// Refuses unavailable/full control delivery; the owner checks layout and clock identity.
    pub fn eq_gain_program(&self, program: EqGainProgram) -> Result<(), QueueError> {
        self.call(QueueCommand::EqGainProgram(program))
    }
}
