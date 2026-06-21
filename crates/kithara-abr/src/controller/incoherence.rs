use kithara_events::AbrEvent;
use kithara_platform::{
    time::{Duration, Instant, sleep},
    tokio,
};
use tokio::task;

use super::{
    core::{AbrController, AbrPeerId},
    peer::PeerEntry,
};

/// Resolved context for `check_incoherence`.
struct IncoherenceCtx {
    progress: kithara_events::AbrProgressSnapshot,
    entry: std::sync::Arc<PeerEntry>,
}

impl AbrController {
    pub(super) fn check_incoherence(
        &self,
        peer_id: AbrPeerId,
        reader_pt_at_switch: Duration,
        switch_at: Instant,
    ) {
        let Some(ctx) = self.resolve_incoherence_ctx(peer_id) else {
            return;
        };
        if ctx.progress.reader_playback_time <= reader_pt_at_switch
            && let Some(bus) = ctx.entry.bus()
        {
            bus.publish(AbrEvent::Incoherence {
                description: format!(
                    "reader stuck at {reader_pt_at_switch:?} after variant switch"
                ),
                elapsed: switch_at.elapsed(),
            });
        }
    }

    /// Single Option-resolver for the four context-resolve guards (peer,
    /// not-locked, peer-still-alive, peer-has-progress). Each `?` chains
    /// into "any None → abort" — the lint accepts this homogeneous form
    /// because the helper compiles to one branch site, not a cascade.
    fn resolve_incoherence_ctx(&self, peer_id: AbrPeerId) -> Option<IncoherenceCtx> {
        let entry = self.peer_entry(peer_id)?;
        if entry.state.as_ref().is_some_and(|s| s.is_locked()) {
            return None;
        }
        let peer = entry.peer_weak.upgrade()?;
        let progress = peer.progress()?;
        Some(IncoherenceCtx { progress, entry })
    }

    pub(crate) fn schedule_incoherence_watch(
        &self,
        peer_id: AbrPeerId,
        reader_pt: Duration,
        now: Instant,
    ) {
        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };
        let token = self.parent_cancel.child();
        {
            let mut slot = entry.last_variant_switch.lock();
            *slot = Some((now, reader_pt));
        }
        {
            let mut slot = entry.incoherence_cancel.lock();
            if let Some(prev) = slot.replace(token.clone()) {
                prev.cancel();
            }
        }

        let deadline = self.settings.incoherence_deadline;
        let controller_weak = self.self_weak.clone();
        task::spawn(async move {
            tokio::select! {
                () = sleep(deadline) => {}
                () = token.cancelled() => return,
            }
            let Some(ctrl) = controller_weak.upgrade() else {
                return;
            };
            ctrl.check_incoherence(peer_id, reader_pt, now);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_platform::{CancelToken, time::Duration};
    use kithara_test_utils::kithara;

    use crate::{Abr, AbrController, AbrSettings};

    struct DummyPeer;
    impl Abr for DummyPeer {}

    /// Contract (W3 Task 3.3): the per-watch incoherence token derives from the
    /// controller's parent cancel (`schedule_incoherence_watch` →
    /// `self.parent_cancel.child()`), so cancelling the parent — player or
    /// downloader teardown — gates the in-flight watch. This pins the public
    /// `cancel` parameter added to `AbrController::new`.
    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn parent_cancel_gates_incoherence_watch() {
        let parent = CancelToken::never();
        let controller = AbrController::new(AbrSettings::default(), parent.clone());
        let peer: Arc<dyn Abr> = Arc::new(DummyPeer);
        let handle = controller.register(&peer);
        let peer_id = handle.peer_id();

        controller.schedule_incoherence_watch(peer_id, Duration::ZERO, Instant::now());
        let token = controller
            .peer_entry(peer_id)
            .expect("peer entry exists after register")
            .incoherence_cancel
            .lock()
            .clone()
            .expect("schedule stores a watch token");

        assert!(
            !token.is_cancelled(),
            "watch token must be live before the parent cancels"
        );
        parent.cancel();
        assert!(
            token.is_cancelled(),
            "parent cancel must gate the incoherence watch token"
        );
    }
}
