use std::sync::{Arc, atomic::Ordering};

use super::super::{
    core::PlayerImpl,
    state::{PendingNext, PendingNextState},
};
use crate::{bridge::PlayerCmd, error::PlayError};

/// Outcome of resolving an arm request under the short phase lock, acted on
/// outside the lock to avoid holding it across `send_to_slot`.
enum ArmDecision {
    /// The same index is already armed; return its src verbatim.
    AlreadyArmed(Arc<str>),
    /// The slot was cleared; optionally unload the previous src.
    Clear(Option<Arc<str>>),
}

struct ActivatedPending {
    src: Arc<str>,
    duration_seconds: f64,
}

impl PlayerImpl {
    /// Mark the armed-next slot at `index` activated under a short lock,
    /// returning its src. `Ok(None)` when it was already activated.
    fn activate_pending(&self, index: usize) -> Result<Option<ActivatedPending>, PlayError> {
        let mut phase = self.phase.lock();
        let pending = phase
            .pending_mut()
            .and_then(|slot| slot.as_mut())
            .ok_or(PlayError::NotReady)?;
        if pending.index != index {
            return Err(PlayError::Internal(format!(
                "commit_next index mismatch: requested {index}, armed {}",
                pending.index
            )));
        }
        let outcome = if pending.state.activated() {
            None
        } else {
            let src = Arc::clone(&pending.src);
            let duration_seconds = pending.duration_seconds;
            pending.state = PendingNextState::ActivatedReady;
            Some(ActivatedPending {
                src,
                duration_seconds,
            })
        };
        drop(phase);
        Ok(outcome)
    }

    /// Load `items[index]` into the audio-thread arena in `Preloading`
    /// state, ready for sample-accurate gapless stitch (cf=0) or parallel
    /// fade (cf>0).
    ///
    /// If a different next is already armed, it is unloaded first.
    /// Idempotent for the same index. Returns `Some(src)` on success;
    /// `None` if `items[index]` is empty (loader hasn't filled it yet) or
    /// `index` is out of range.
    pub fn arm_next(&self, index: usize) -> Option<Arc<str>> {
        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if index >= self.item_count() {
            return None;
        }

        // Resolve any already-armed slot under a short phase lock: either the
        // same index is already armed (return early), or it must be cleared
        // and possibly unloaded outside the lock.
        let mut phase = self.phase.lock();
        let existing = phase.pending_mut().and_then(|slot| slot.as_ref());
        let decision = match existing {
            Some(existing) if existing.index == index => {
                ArmDecision::AlreadyArmed(Arc::clone(&existing.src))
            }
            Some(existing) => {
                let unload = (!(existing.state.activated() && existing.index == current_index))
                    .then(|| Arc::clone(&existing.src));
                if let Some(slot) = phase.pending_mut() {
                    *slot = None;
                }
                ArmDecision::Clear(unload)
            }
            None => ArmDecision::Clear(None),
        };
        drop(phase);

        let to_unload = match decision {
            ArmDecision::AlreadyArmed(src) => return Some(src),
            ArmDecision::Clear(unload) => unload,
        };
        if let Some(prev_src) = to_unload {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: prev_src });
        }

        let (src, duration_seconds) = self.enqueue_to_processor(index)?;
        if let Some(pending_slot) = self.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                index,
                duration_seconds,
                src: Arc::clone(&src),
                state: PendingNextState::Armed,
            });
        }
        Some(src)
    }

    /// Snapshot of the armed-next index. `None` when no slot is armed
    /// (or after `commit_next` has consumed it for the current handover).
    #[must_use]
    pub fn armed_next(&self) -> Option<usize> {
        self.phase
            .lock()
            .pending()
            .filter(|pending| !pending.state.activated())
            .map(|pending| pending.index)
    }

    /// Commit the previously armed next track and start the cross-fade.
    ///
    /// Sends `FadeIn` to the audio thread for the armed slot, updates
    /// `current_index`, and publishes `CurrentItemChanged`.
    ///
    /// # Errors
    /// - [`PlayError::NotReady`] if no slot is armed.
    /// - [`PlayError::Internal`] if `index` does not match
    ///   [`Self::armed_next`].
    pub fn commit_next(&self, index: usize) -> Result<(), PlayError> {
        // `None` ⇒ the slot was already activated (idempotent no-op).
        let Some(activated) = self.activate_pending(index)? else {
            return Ok(());
        };

        self.start_playback(activated.src);
        self.publish_current_track_snapshot(activated.duration_seconds);
        let current_index = self.core.current_index.load(Ordering::Relaxed);
        if index != current_index {
            self.core.current_index.store(index, Ordering::Relaxed);
            self.announce_current_item(index);
        }
        Ok(())
    }

    /// Drop the armed next slot without committing.
    ///
    /// Sends `UnloadTrack` to the audio thread for the armed src and
    /// clears the pending slot. Skips the unload if the armed slot has
    /// already been activated for the current index (the activated track
    /// is now the leading one — unloading would silence playback).
    pub fn unarm_next(&self) {
        self.unarm_next_internal(Some(self.core.current_index.load(Ordering::Relaxed)));
    }

    pub(crate) fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        let pending = self.phase.lock().pending_mut().and_then(Option::take);
        let Some(pending) = pending else {
            return;
        };
        let preserve_active_current = current_index_hint
            .is_some_and(|index| pending.state.activated() && pending.index == index);
        if !preserve_active_current {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { src: pending.src });
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{Event, PlayerEvent};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{player::PlayerConfig, session::testing};

    #[kithara::test]
    fn commit_next_without_arm_returns_not_ready() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player.commit_next(1).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn commit_next_publishes_snapshot_before_current_item_changed() {
        let player = PlayerImpl::new(PlayerConfig {
            session: Some(testing::test_session()),
            ..PlayerConfig::default()
        });
        player
            .ensure_engine_started()
            .expect("engine start must succeed");
        player.ensure_slot().expect("slot allocation must succeed");
        let mut rx = player.subscribe();

        if let Some(pending_slot) = player.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                src: Arc::from("next.mp3"),
                state: PendingNextState::Armed,
                index: 1,
                duration_seconds: 162.0,
            });
        }

        player.commit_next(1).expect("commit_next must succeed");

        assert!(matches!(
            rx.try_recv(),
            Ok(Event::Player(PlayerEvent::CurrentItemChanged))
        ));
        assert_eq!(player.duration_seconds(), Some(162.0));
    }
}
