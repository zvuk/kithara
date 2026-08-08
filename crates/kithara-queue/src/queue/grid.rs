use kithara_events::TrackId;
use kithara_play::{BeatQuantum, BeatStart, PlayerImpl, TrackAnalysis, TrackBeat};
use tracing::warn;

use super::Queue;
use crate::{attempts::LoadClass, error::QueueError};

impl Queue {
    /// Places this deck on the session grid and starts the item at `index` on
    /// the next beat that lands on `quantum`.
    ///
    /// The order is the whole contract. A resource is timed by the tempo slot
    /// in force when it is *built*, so a deck bound after its track was loaded
    /// would start on the beat and then run at its own tempo. This binds
    /// first, rebuilds the track's resource under the binding, and stamps the
    /// start only once that resource is in the processor — a stamp sent to a
    /// track the processor does not hold is silently dropped.
    ///
    /// Rebuilding is an ordinary load from the track's own source, the same
    /// path a respawn takes; it is not a decoder recreate.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError`] when `index` names no track, and forwards
    /// [`PlayError`] when the session has committed no grid or the analysis
    /// carries no usable map.
    pub fn start_at_beat(
        &self,
        index: usize,
        analysis: &TrackAnalysis,
        track_anchor: TrackBeat,
        quantum: BeatQuantum,
    ) -> Result<(), QueueError> {
        let found = {
            let guard = self.lock_tracks();
            let found = guard
                .get(index)
                .map(|entry| (entry.id, entry.source.clone()));
            drop(guard);
            found
        };
        let (id, source) =
            found.ok_or_else(|| QueueError::InvalidUrl(format!("no track at index {index}")))?;
        let start = self
            .player
            .bind_to_grid(analysis, track_anchor, quantum)
            .map_err(QueueError::from)?;
        self.set_pending_beat_start(id, start);
        self.spawn_apply_after_load(id, source, LoadClass::Interactive);
        Ok(())
    }

    pub(super) fn set_pending_beat_start(&self, id: TrackId, start: BeatStart) {
        *self.lock_pending_beat_start() = Some((id, start));
    }
}

/// Arms the freshly built resource and stamps its start.
///
/// Called from the load-landing path, where the resource is already in the
/// player's item slot. Arming is what moves it into the processor as
/// preloading, and only a preloading track can be started on a stamped beat.
pub(super) fn arm_landed_beat_start(
    player: &PlayerImpl,
    index: usize,
    id: TrackId,
    start: BeatStart,
) {
    if let Err(error) = player.arm_at_beat(index, start) {
        warn!(id = id.as_u64(), %error, "the beat-anchored start could not be armed");
    }
}
