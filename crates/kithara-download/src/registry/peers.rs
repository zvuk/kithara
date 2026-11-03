use std::{sync::atomic::Ordering, task::Poll};

use kithara_abr::AbrPeerId;
use kithara_events::EventBus;
use kithara_platform::{CancelGroup, CancelToken, sync::Arc, time::Instant, tokio::sync::mpsc};
use thunderdome::{Arena, Index};

use super::slots::{Slots, slot_index};
use crate::{
    RequestPriority,
    downloader::{DownloaderInner, RegisteredPeerEntry},
    peer::{InternalCmd, Peer, ResponseTarget, SlotEntry},
};

/// Activity accumulated by [`super::core::Registry::tick`] so the downloader can classify
/// real forward motion rather than a spurious `poll_fn` wake.
#[derive(Default, Clone, Copy)]
pub(super) struct PollStats {
    pub(super) abr_ticked: bool,
    pub(super) drained_cmds: usize,
    pub(super) peer_batches: usize,
}

/// Per-peer entry in the registry.
struct PeerEntry {
    /// ABR peer id stamped on every proactively-scheduled `InternalCmd`
    /// so the Downloader can credit bandwidth samples when the fetch
    /// completes.
    peer_id: AbrPeerId,
    /// Event destination captured before registration becomes visible.
    bus: Option<EventBus>,
    peer: Arc<dyn Peer>,
    peer_cancel: CancelToken,
    cmd_rx: mpsc::Receiver<InternalCmd>,
    peer_done: bool,
}

/// Owns registered peers and their imperative/proactive command inputs.
#[derive(Default)]
pub(super) struct Peers {
    entries: Arena<PeerEntry>,
    last_admitted: Option<Index>,
}

impl Peers {
    pub(super) fn priority(&self, peer: Index) -> Option<RequestPriority> {
        self.entries.get(peer).map(|entry| entry.peer.priority())
    }

    /// Register a new peer.
    ///
    /// The peer's `peer_cancel` is the [`crate::peer::PeerHandle`]'s own cancel token
    /// (carried through `RegisteredPeerEntry::cancel`). When the last
    /// `PeerHandle` clone drops, `PeerInner::Drop` fires that token and
    /// this registry detects the cancellation on its next `poll_peers`
    /// pass, removing the peer entry (and releasing its `Arc<dyn Peer>`).
    /// Without this, the Registry held a sibling child token of the
    /// whole-Downloader cancel and never saw per-peer drops — the peer
    /// Arc would leak until the entire Downloader shut down.
    pub(super) fn add(&mut self, entry: RegisteredPeerEntry) {
        self.entries.insert(PeerEntry {
            peer: entry.peer,
            cmd_rx: entry.cmd_rx,
            peer_cancel: entry.cancel,
            peer_done: false,
            bus: entry.bus,
            peer_id: entry.peer_id,
        });
    }

    /// Poll all peers: drain `cmd_rx` channels and call `poll_next`.
    /// Route each command to the correct slot. Returns per-peer counters
    /// so [`super::core::Registry::tick`] can classify forward motion.
    pub(super) fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
        inner: &DownloaderInner,
        slots: &mut Slots,
    ) -> PollStats {
        let mut to_remove: Vec<Index> = Vec::new();
        let mut stats = PollStats::default();

        inner.fetch_waker.register(cx.waker());

        let mut order: Vec<Index> = self.entries.iter().map(|(index, _)| index).collect();
        if let Some(last) = self.last_admitted
            && let Some(position) = order.iter().position(|index| *index == last)
        {
            let next = (position + 1) % order.len();
            order.rotate_left(next);
        }
        order.sort_by_key(|index| self.priority(*index) != Some(RequestPriority::High));
        for idx in order {
            let Some(entry) = self.entries.get_mut(idx) else {
                continue;
            };
            if entry.peer_cancel.is_cancelled() {
                to_remove.push(idx);
            }
            while let Poll::Ready(Some(mut cmd)) = entry.cmd_rx.poll_recv(cx) {
                let peer_prio = entry.peer.priority();
                let slot = slot_index(peer_prio, cmd.effective_priority());
                cmd.peer = Some(idx);
                let entry_slot = SlotEntry {
                    cmd,
                    peer_cancel: entry.peer_cancel.clone(),
                };
                slots.enqueue(slot, entry_slot);
                stats.drained_cmds += 1;
            }

            if entry.peer_done {
                continue;
            }

            let budget = inner
                .config
                .max_concurrent
                .saturating_sub(inner.inflight.load(Ordering::Relaxed))
                .saturating_sub(slots.len());
            match entry.peer.poll_next(cx, budget) {
                Poll::Ready(Some(batch)) => {
                    let peer_prio = entry.peer.priority();
                    let bus = entry.bus.clone();
                    let batch_had_cmds = !batch.is_empty();
                    for cmd in batch {
                        let epoch_cancel = cmd.cancel.clone();
                        let cancel = match epoch_cancel {
                            Some(epoch) => CancelGroup::new(vec![entry.peer_cancel.clone(), epoch]),
                            None => CancelGroup::new(vec![entry.peer_cancel.child()]),
                        };
                        let cmd_prio = cmd.priority.unwrap_or(RequestPriority::Low);
                        let request_id = inner.next_request_id();
                        let enqueued_at = Instant::now();
                        let internal = InternalCmd {
                            cmd,
                            cancel,
                            request_id,
                            enqueued_at,
                            priority: cmd_prio,
                            response: ResponseTarget::Streaming,
                            peer: Some(idx),
                            bus: bus.clone(),
                            peer_id: entry.peer_id,
                        };
                        let slot = slot_index(peer_prio, internal.effective_priority());
                        let entry_slot = SlotEntry {
                            cmd: internal,
                            peer_cancel: entry.peer_cancel.clone(),
                        };
                        slots.enqueue(slot, entry_slot);
                    }
                    if batch_had_cmds {
                        self.last_admitted = Some(idx);
                        stats.peer_batches += 1;
                    }
                }
                Poll::Ready(None) => {
                    entry.peer_done = true;
                }
                Poll::Pending => {}
            }
        }

        for idx in to_remove {
            self.entries.remove(idx);
        }

        stats
    }
}
