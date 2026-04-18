//! Peer registry with 2×2 priority slot map.

use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{Arc, atomic::Ordering},
    task::Poll,
};

use kithara_events::EventBus;
use kithara_platform::{CancelGroup, RwLock, tokio, tokio::sync::mpsc};
use thunderdome::{Arena, Index};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::{
    batch::BatchGroup,
    cmd::Priority,
    downloader::{DownloaderInner, RegisteredPeerEntry},
    peer::{InternalCmd, Peer, ResponseTarget},
};

const SLOT_COUNT: usize = 4;

#[repr(usize)]
enum Slot {
    HighHigh = 0,
    HighLow = 1,
    LowHigh = 2,
    LowLow = 3,
}

/// Map (`peer_priority`, `cmd_priority`) → slot index.
/// Processing order: 0 → 1 → 2 → 3.
fn slot_index(peer_prio: Priority, cmd_prio: Priority) -> usize {
    match (peer_prio, cmd_prio) {
        (Priority::High, Priority::High) => Slot::HighHigh as usize,
        (Priority::High, Priority::Low) => Slot::HighLow as usize,
        (Priority::Low, Priority::High) => Slot::LowHigh as usize,
        (Priority::Low, Priority::Low) => Slot::LowLow as usize,
    }
}

/// Per-peer entry in the registry.
struct PeerEntry {
    peer: Arc<dyn Peer>,
    cmd_rx: mpsc::Receiver<InternalCmd>,
    peer_cancel: CancellationToken,
    peer_done: bool,
    /// Same Arc as the owning [`PeerHandle`]'s bus. Read-snapshot on
    /// every proactive poll so `PeerHandle::with_bus` takes effect
    /// immediately.
    bus: Arc<RwLock<Option<EventBus>>>,
}

/// Peer registry: owns peers, routes commands to priority slots,
/// and drives batch execution.
pub(super) struct Registry {
    peers: Arena<PeerEntry>,
    slots: [VecDeque<InternalCmd>; SLOT_COUNT],
    urgent_notify: Arc<Notify>,
}

impl Registry {
    pub(super) fn new() -> Self {
        Self {
            peers: Arena::new(),
            slots: Default::default(),
            urgent_notify: Arc::new(Notify::new()),
        }
    }

    /// Register a new peer.
    ///
    /// The peer's `peer_cancel` is the [`PeerHandle`]'s own cancel token
    /// (carried through `RegisteredPeerEntry::cancel`). When the last
    /// `PeerHandle` clone drops, `PeerInner::Drop` fires that token and
    /// this registry detects the cancellation on its next `poll_peers`
    /// pass, removing the peer entry (and releasing its `Arc<dyn Peer>`).
    /// Without this, the Registry held a sibling child token of the
    /// whole-Downloader cancel and never saw per-peer drops — the peer
    /// Arc would leak until the entire Downloader shut down.
    pub(super) fn add(&mut self, entry: RegisteredPeerEntry) -> Index {
        let idx = self.peers.insert(PeerEntry {
            peer: entry.peer,
            cmd_rx: entry.cmd_rx,
            peer_cancel: entry.cancel,
            peer_done: false,
            bus: entry.bus,
        });
        self.urgent_notify.notify_one();
        idx
    }

    /// Poll all peers: drain `cmd_rx` channels and call `poll_next`.
    /// Route each command to the correct slot.
    fn poll_peers(&mut self, cx: &mut std::task::Context<'_>, inner: &DownloaderInner) {
        let mut to_remove: Vec<Index> = Vec::new();

        // Register the global fetch_waker so we wake when inflight drops.
        inner.fetch_waker.register(cx.waker());

        for (idx, entry) in &mut self.peers {
            // Drain imperative commands from PeerHandle::execute/batch.
            while let Poll::Ready(Some(mut cmd)) = entry.cmd_rx.poll_recv(cx) {
                let peer_prio = entry.peer.priority();
                let slot = slot_index(peer_prio, cmd.priority);
                cmd.peer = Some(idx);
                self.slots[slot].push_back(cmd);
                if slot <= 1 {
                    self.urgent_notify.notify_one();
                }
            }

            // Skip peer_done or global capacity reached.
            if entry.peer_done {
                continue;
            }
            if inner.inflight.load(Ordering::Relaxed) >= inner.max_concurrent {
                continue;
            }

            // Poll peer's proactive stream.
            match entry.peer.poll_next(cx) {
                Poll::Ready(Some(batch)) => {
                    let peer_prio = entry.peer.priority();
                    let bus = entry.bus.lock_sync_read().clone();
                    for mut cmd in batch {
                        let epoch_cancel = cmd.cancel.take();
                        let cancel = match epoch_cancel {
                            Some(epoch) => CancelGroup::new(vec![entry.peer_cancel.clone(), epoch]),
                            None => CancelGroup::new(vec![entry.peer_cancel.child_token()]),
                        };
                        let cmd_prio = Priority::Low;
                        let slot = slot_index(peer_prio, cmd_prio);
                        let internal = InternalCmd {
                            cmd,
                            cancel,
                            priority: cmd_prio,
                            response: ResponseTarget::Streaming,
                            peer: Some(idx),
                            bus: bus.clone(),
                        };
                        self.slots[slot].push_back(internal);
                        if slot <= 1 {
                            self.urgent_notify.notify_one();
                        }
                    }
                }
                Poll::Ready(None) => {
                    entry.peer_done = true;
                }
                Poll::Pending => {}
            }
        }

        // Remove dead peers. `peer_cancel` is the handle's own cancel
        // token — it fires from `PeerInner::Drop`, so observing
        // `is_cancelled()` here is sufficient. The previous gate
        // required `peer_done && peer_cancel.is_cancelled()`, but
        // peers with indefinite streams (e.g. `HlsPeer::poll_next`
        // never returns `Ready(None)`) would never meet it and leaked.
        for (idx, entry) in &self.peers {
            if entry.peer_cancel.is_cancelled() {
                to_remove.push(idx);
            }
        }
        for idx in to_remove {
            self.peers.remove(idx);
        }
    }

    /// Single tick: poll peers, process urgent, then demand with throttle.
    ///
    /// Accepts `register_rx` so that new peer registrations are handled
    /// inside `poll_fn` rather than in a competing `select!` arm.
    /// This guarantees that `process()` runs to completion — a `select!`
    /// branch for registrations would drop `tick()` mid-batch, losing
    /// unspawned `FetchCmd`s.
    pub(super) async fn tick(
        &mut self,
        inner: &DownloaderInner,
        register_rx: &mut mpsc::UnboundedReceiver<RegisteredPeerEntry>,
    ) {
        // Wait until at least one slot has work.
        poll_fn(|cx| {
            // Drain registrations inside the poll loop so new peers
            // wake poll_fn without interrupting batch execution.
            while let Poll::Ready(Some(entry)) = register_rx.poll_recv(cx) {
                self.add(entry);
            }
            self.poll_peers(cx, inner);
            if self.has_slot_work() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        // 1. Urgent: drain slots [0],[1].
        let mut urgent_cmds: Vec<InternalCmd> = Vec::new();
        urgent_cmds.extend(self.slots[0].drain(..));
        urgent_cmds.extend(self.slots[1].drain(..));
        let urgent_batch = BatchGroup::from_iter(urgent_cmds.into_iter());
        if !urgent_batch.is_empty() {
            urgent_batch.process(inner).await;
            return;
        }

        // 2. Demand: drain slots [2],[3].
        if !inner.demand_throttle.is_zero() {
            tokio::select! {
                () = tokio::time::sleep(inner.demand_throttle) => {}
                () = self.urgent_notify.notified() => { return; }
            }
        }

        let mut demand_cmds: Vec<InternalCmd> = Vec::new();
        demand_cmds.extend(self.slots[Slot::LowHigh as usize].drain(..));
        demand_cmds.extend(self.slots[Slot::LowLow as usize].drain(..));
        let demand_batch = BatchGroup::from_iter(demand_cmds.into_iter());
        if !demand_batch.is_empty() {
            demand_batch.process(inner).await;
        }
    }

    fn has_slot_work(&self) -> bool {
        self.slots.iter().any(|s| !s.is_empty())
    }

    /// Check `peer.priority()` for each command in slots,
    /// move to the correct slot if priority changed.
    pub(super) fn reschedule(&mut self) {
        let mut moves: Vec<(usize, usize, usize)> = Vec::new();
        let mut cancels: Vec<(usize, usize)> = Vec::new();

        for slot_idx in 0..SLOT_COUNT {
            for (i, cmd) in self.slots[slot_idx].iter().enumerate() {
                let Some(peer_idx) = cmd.peer else {
                    continue;
                };
                let Some(entry) = self.peers.get(peer_idx) else {
                    cancels.push((slot_idx, i));
                    continue;
                };
                let correct_slot = slot_index(entry.peer.priority(), cmd.priority);
                if correct_slot != slot_idx {
                    moves.push((slot_idx, i, correct_slot));
                }
            }
        }

        cancels.sort_by(|a, b| b.1.cmp(&a.1));
        for (slot_idx, i) in cancels {
            if let Some(cmd) = self.slots[slot_idx].remove(i) {
                super::batch::deliver_cancelled(cmd.response, cmd.cmd);
            }
        }

        moves.sort_by(|a, b| b.1.cmp(&a.1));
        for (from_slot, i, to_slot) in moves {
            if let Some(cmd) = self.slots[from_slot].remove(i) {
                self.slots[to_slot].push_back(cmd);
                if to_slot <= 1 {
                    self.urgent_notify.notify_one();
                }
            }
        }
    }
}
