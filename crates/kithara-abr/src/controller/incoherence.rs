//! Incoherence watcher — fires `AbrEvent::Incoherence` if the reader has
//! not advanced within `incoherence_deadline` after a variant switch.

use kithara_events::AbrEvent;
use kithara_platform::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

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

    pub(super) fn schedule_incoherence_watch(
        &self,
        peer_id: AbrPeerId,
        reader_pt: Duration,
        now: Instant,
    ) {
        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };
        let token = CancellationToken::new();
        {
            let mut slot = entry.last_variant_switch.lock_sync();
            *slot = Some((now, reader_pt));
        }
        {
            let mut slot = entry.incoherence_cancel.lock_sync();
            if let Some(prev) = slot.replace(token.clone()) {
                prev.cancel();
            }
        }

        let deadline = self.settings.incoherence_deadline;
        let controller_weak = self.self_weak.clone();
        kithara_platform::tokio::spawn(async move {
            kithara_platform::tokio::select! {
                () = kithara_platform::tokio::time::sleep(deadline) => {}
                () = token.cancelled() => return,
            }
            let Some(ctrl) = controller_weak.upgrade() else {
                return;
            };
            ctrl.check_incoherence(peer_id, reader_pt, now);
        });
    }
}
