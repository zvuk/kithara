use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_play::SlotGainProgram;

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
}
