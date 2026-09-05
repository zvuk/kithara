use std::sync::PoisonError;

use kithara_bufpool::HasPool;
use kithara_events::{AdvanceReason, QueueEvent, TrackId, TrackStatus};
use kithara_platform::tokio::task;
use kithara_play::{Resource, SelectTransition};
use tracing::{debug, warn};

use crate::{
    error::QueueError,
    queue::{QueueControl, types::SelectPhase},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(super) fn watch_apply(
        &self,
        id: TrackId,
        handle: Option<task::JoinHandle<Result<Resource, QueueError>>>,
    ) {
        if self.is_closed() {
            return;
        }
        let Some(handle) = handle else {
            return;
        };
        let queue = self.clone();
        drop(task::spawn(async move {
            let resource = match handle.await {
                Ok(Ok(resource)) => resource,
                Ok(Err(_)) => return,
                Err(join_err) => {
                    warn!(id = id.as_u64(), error = %join_err, "loader join failed");
                    return;
                }
            };
            drop(task::spawn_blocking(move || {
                queue.apply_loaded(id, resource);
            }));
        }));
    }

    /// Apply a finished load synchronously, off the runtime.
    ///
    /// Takes the admission lock and dispatches through the session's
    /// synchronous command bridge, so the caller waits for a reply. On a
    /// runtime worker that wait parks the executor thread.
    fn apply_loaded(&self, id: TrackId, resource: Resource) {
        let _admission = self.lock_admission();
        if self.is_closed() {
            return;
        }

        // WHY: Held across the whole synchronous block (never across .await): the Cancelled re-check and select_item must be atomic w.r.t. a
        let _apply = self
            .select_apply
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        if self.player.is_closed() {
            return;
        }

        let was_cancelled = self
            .tracks
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .is_some_and(|entry| matches!(entry.status, TrackStatus::Cancelled));
        if was_cancelled {
            debug!(
                id = id.as_u64(),
                "load was overridden by a later select; skipping replace_item"
            );
            return;
        }

        let index = {
            let guard = self.tracks.lock();
            guard.iter().position(|entry| entry.id == id)
        };
        let Some(index) = index else {
            debug!(
                id = id.as_u64(),
                "load completed but track no longer in queue"
            );
            return;
        };

        if let Err(error) = self.player.replace_item(index, resource, id) {
            debug!(id = id.as_u64(), %error, "player closed before load could be applied");
            return;
        }
        self.tracks.set_status(id, TrackStatus::Loaded);
        if self
            .tracks
            .lock()
            .get(index)
            .is_some_and(|entry| entry.id == id)
        {
            self.bus.publish(QueueEvent::NextTrackReady { id, index });
        }

        let pending_transition = {
            let mut phase = self
                .pending_select
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let result = match *phase {
                SelectPhase::Pending(pending) if pending.id == id => {
                    *phase = SelectPhase::Idle;
                    Some(pending.transition)
                }
                _ => None,
            };
            drop(phase);
            result
        };

        let Some(transition) = pending_transition else {
            return;
        };
        let was_playing = self.player.is_playing();
        let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
        if was_playing && crossfade > 0.0 {
            self.bus.publish(QueueEvent::CrossfadeStarted {
                duration_seconds: crossfade,
            });
        }
        if let Err(error) = self.player.select_item_with_crossfade(
            index,
            SelectTransition {
                autoplay: true,
                crossfade_seconds: crossfade,
            },
        ) {
            warn!(id = id.as_u64(), error = %error, "pending select failed");
            return;
        }
        self.navigation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .select(index);
        self.bus.publish(QueueEvent::CurrentTrackAdvance {
            id: Some(id),
            reason: AdvanceReason::UserSelect,
        });
        self.tracks.set_status(id, TrackStatus::Consumed);
    }
}
