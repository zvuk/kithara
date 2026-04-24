//! Peer registry with 2×2 priority slot map.

use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{Arc, atomic::Ordering},
    task::Poll,
};

use kithara_abr::AbrPeerId;
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

/// Observable forward motion of the fetch pipeline across one
/// [`Registry::tick`]. Consumed by the hang watchdog in
/// [`Downloader::run`](super::downloader::Downloader::run) to distinguish
/// legitimate quiet periods from genuine deadlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FetchProgress {
    /// No in-flight fetches and no queued work. Legitimate quiet; the
    /// watchdog must stay silent.
    Idle,
    /// At least one of: a command was drained from a peer's channel, a
    /// peer yielded a batch, an in-flight fetch completed (inflight
    /// decremented), or a new fetch was dispatched this tick.
    Advanced,
    /// Pending work exists (inflight > 0 or slots non-empty) but this
    /// tick observed no forward motion. Consecutive stalls across the
    /// watchdog window trigger a deadlock panic.
    Stalled,
}

/// Counters returned from [`Registry::poll_peers`] so [`Registry::tick`]
/// can classify the tick as [`FetchProgress::Advanced`] when work actually
/// moved (channel drain or peer batch) rather than just when `poll_fn` was
/// woken spuriously by the `fetch_waker`.
#[derive(Default, Clone, Copy)]
struct PollStats {
    drained_cmds: usize,
    peer_batches: usize,
}

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
    /// ABR peer id stamped on every proactively-scheduled `InternalCmd`
    /// so the Downloader can credit bandwidth samples when the fetch
    /// completes.
    peer_id: AbrPeerId,
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
            peer_id: entry.peer_id,
        });
        self.urgent_notify.notify_one();
        idx
    }

    /// Poll all peers: drain `cmd_rx` channels and call `poll_next`.
    /// Route each command to the correct slot. Returns per-peer counters
    /// so [`tick`](Self::tick) can classify forward motion.
    fn poll_peers(
        &mut self,
        cx: &mut std::task::Context<'_>,
        inner: &DownloaderInner,
    ) -> PollStats {
        let mut to_remove: Vec<Index> = Vec::new();
        let mut stats = PollStats::default();

        // Register the global fetch_waker so we wake when inflight drops.
        inner.fetch_waker.register(cx.waker());

        for (idx, entry) in &mut self.peers {
            // Drain imperative commands from PeerHandle::execute/batch.
            while let Poll::Ready(Some(mut cmd)) = entry.cmd_rx.poll_recv(cx) {
                let peer_prio = entry.peer.priority();
                let slot = slot_index(peer_prio, cmd.priority);
                cmd.peer = Some(idx);
                self.slots[slot].push_back(cmd);
                stats.drained_cmds += 1;
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
                    let batch_had_cmds = !batch.is_empty();
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
                            peer_id: entry.peer_id,
                        };
                        self.slots[slot].push_back(internal);
                        if slot <= 1 {
                            self.urgent_notify.notify_one();
                        }
                    }
                    if batch_had_cmds {
                        stats.peer_batches += 1;
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

        stats
    }

    /// Single tick: poll peers, process urgent, then demand with throttle.
    ///
    /// Accepts `register_rx` so that new peer registrations are handled
    /// inside `poll_fn` rather than in a competing `select!` arm.
    /// This guarantees that `process()` runs to completion — a `select!`
    /// branch for registrations would drop `tick()` mid-batch, losing
    /// unspawned `FetchCmd`s.
    ///
    /// Returns a [`FetchProgress`] describing whether fetch work moved
    /// forward this tick. Idle returns are possible when `poll_fn` is
    /// woken by `fetch_waker` (an in-flight fetch completed elsewhere)
    /// but no new peer/command activity occurred; the downloader
    /// watchdog uses this signal to avoid false panics during quiet
    /// periods.
    pub(super) async fn tick(
        &mut self,
        inner: &DownloaderInner,
        register_rx: &mut mpsc::UnboundedReceiver<RegisteredPeerEntry>,
    ) -> FetchProgress {
        let inflight_enter = inner.inflight.load(Ordering::Relaxed);
        let mut aggregate = PollStats::default();

        // Wait until at least one slot has work.
        poll_fn(|cx| {
            // Drain registrations inside the poll loop so new peers
            // wake poll_fn without interrupting batch execution.
            while let Poll::Ready(Some(entry)) = register_rx.poll_recv(cx) {
                self.add(entry);
            }
            let stats = self.poll_peers(cx, inner);
            aggregate.drained_cmds += stats.drained_cmds;
            aggregate.peer_batches += stats.peer_batches;
            if self.has_slot_work() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        let mut dispatched: usize = 0;

        // 1. Urgent: drain slots [0],[1].
        let mut urgent_cmds: Vec<InternalCmd> = Vec::new();
        urgent_cmds.extend(self.slots[0].drain(..));
        urgent_cmds.extend(self.slots[1].drain(..));
        let urgent_batch = BatchGroup::from_iter(urgent_cmds.into_iter());
        if !urgent_batch.is_empty() {
            dispatched += urgent_batch.process(inner).await;
            return classify_progress(
                inflight_enter,
                inner.inflight.load(Ordering::Relaxed),
                aggregate,
                dispatched,
            );
        }

        // 2. Demand: drain slots [2],[3].
        if !inner.demand_throttle.is_zero() {
            let preempted_by_urgent = tokio::select! {
                () = tokio::time::sleep(inner.demand_throttle) => false,
                () = self.urgent_notify.notified() => true,
            };
            if preempted_by_urgent {
                return classify_progress(
                    inflight_enter,
                    inner.inflight.load(Ordering::Relaxed),
                    aggregate,
                    dispatched,
                );
            }
        }

        let mut demand_cmds: Vec<InternalCmd> = Vec::new();
        demand_cmds.extend(self.slots[Slot::LowHigh as usize].drain(..));
        demand_cmds.extend(self.slots[Slot::LowLow as usize].drain(..));
        let demand_batch = BatchGroup::from_iter(demand_cmds.into_iter());
        if !demand_batch.is_empty() {
            dispatched += demand_batch.process(inner).await;
        }

        classify_progress(
            inflight_enter,
            inner.inflight.load(Ordering::Relaxed),
            aggregate,
            dispatched,
        )
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

/// Classify a completed [`Registry::tick`] as one of the [`FetchProgress`]
/// variants. `inflight_enter` is the snapshot taken at the start of the
/// tick; `inflight_exit` is the value after processing. An exit below
/// enter means at least one fetch completed while this tick was running
/// — which counts as forward motion regardless of whether any new cmds
/// were drained or batches dispatched.
///
/// Pure function of its inputs so the invariant can be exhaustively
/// tested without constructing a live Downloader.
fn classify_progress(
    inflight_enter: usize,
    inflight_exit: usize,
    poll_stats: PollStats,
    dispatched: usize,
) -> FetchProgress {
    let advanced = poll_stats.drained_cmds > 0
        || poll_stats.peer_batches > 0
        || dispatched > 0
        || inflight_exit < inflight_enter;
    if advanced {
        return FetchProgress::Advanced;
    }
    if inflight_exit > 0 {
        return FetchProgress::Stalled;
    }
    FetchProgress::Idle
}

#[cfg(test)]
mod classify_progress_tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::{FetchProgress, PollStats, classify_progress};

    fn stats(drained: usize, batches: usize) -> PollStats {
        PollStats {
            drained_cmds: drained,
            peer_batches: batches,
        }
    }

    // Idle: no inflight either side, no peer activity, no dispatch.
    #[kithara::test]
    #[case(0, 0)]
    #[case(0, 0)] // exact duplicate — documents invariance under repeat
    fn idle_when_no_work_anywhere(#[case] inflight_enter: usize, #[case] inflight_exit: usize) {
        let out = classify_progress(inflight_enter, inflight_exit, stats(0, 0), 0);
        assert_eq!(out, FetchProgress::Idle);
    }

    // Advanced: any one of drained_cmds / peer_batches / dispatched
    // non-zero flips the outcome to Advanced even with zero inflight.
    #[kithara::test]
    #[case(1, 0, 0)] // drained a cmd
    #[case(0, 1, 0)] // peer yielded a batch
    #[case(0, 0, 1)] // dispatched a fetch
    #[case(3, 0, 0)] // multiple drains
    #[case(0, 2, 0)] // multiple batches
    #[case(0, 0, 5)] // multiple dispatches
    #[case(2, 3, 4)] // all three
    fn advanced_when_any_activity_counter_positive(
        #[case] drained: usize,
        #[case] batches: usize,
        #[case] dispatched: usize,
    ) {
        let out = classify_progress(0, 0, stats(drained, batches), dispatched);
        assert_eq!(out, FetchProgress::Advanced);
    }

    // Advanced: inflight strictly decreasing means a fetch completed
    // this tick — that IS progress regardless of counters.
    #[kithara::test]
    #[case(1, 0)]
    #[case(5, 4)]
    #[case(10, 1)]
    #[case(usize::MAX, usize::MAX - 1)]
    fn advanced_when_inflight_decreases(
        #[case] inflight_enter: usize,
        #[case] inflight_exit: usize,
    ) {
        let out = classify_progress(inflight_enter, inflight_exit, stats(0, 0), 0);
        assert_eq!(out, FetchProgress::Advanced);
    }

    // Stalled: pending work (inflight > 0) at exit, no advancement
    // signals, and inflight did not decrease.
    #[kithara::test]
    #[case(1, 1)] // single stuck fetch
    #[case(5, 5)] // five stuck
    #[case(0, 3)] // new fetches spawned but didn't dispatch; can't happen
    // in practice but classifier must still reflect "there's
    // work we didn't count" — here inflight_exit > 0 && no
    // advancement counters → Stalled, which is correct:
    // someone OTHER than this tick bumped inflight; we can't
    // claim advance.
    fn stalled_when_inflight_stuck_and_no_counters(
        #[case] inflight_enter: usize,
        #[case] inflight_exit: usize,
    ) {
        let out = classify_progress(inflight_enter, inflight_exit, stats(0, 0), 0);
        assert_eq!(out, FetchProgress::Stalled);
    }

    // Advancement beats inflight_exit>0: even with pending fetches, a
    // positive activity counter means the tick made progress.
    #[kithara::test]
    #[case(5, 5, 1, 0, 0)] // drained during stuck inflight
    #[case(5, 5, 0, 1, 0)] // batch emitted during stuck inflight
    #[case(5, 5, 0, 0, 1)] // new fetch dispatched on top of stuck ones
    #[case(5, 6, 0, 0, 1)] // dispatched: inflight went up but advance counted
    fn advanced_dominates_stalled_when_both_signals_present(
        #[case] inflight_enter: usize,
        #[case] inflight_exit: usize,
        #[case] drained: usize,
        #[case] batches: usize,
        #[case] dispatched: usize,
    ) {
        let out = classify_progress(
            inflight_enter,
            inflight_exit,
            stats(drained, batches),
            dispatched,
        );
        assert_eq!(out, FetchProgress::Advanced);
    }

    // Boundary: if inflight_enter > inflight_exit but also counters are
    // zero, the inflight-decrement alone is sufficient for Advanced.
    #[kithara::test]
    fn inflight_decrement_alone_yields_advanced() {
        assert_eq!(
            classify_progress(3, 2, stats(0, 0), 0),
            FetchProgress::Advanced
        );
    }

    // Boundary: inflight_enter == inflight_exit == 0, any single
    // counter positive → Advanced (not Idle).
    #[kithara::test]
    fn single_counter_wins_over_empty_inflight() {
        assert_eq!(
            classify_progress(0, 0, stats(1, 0), 0),
            FetchProgress::Advanced
        );
        assert_eq!(
            classify_progress(0, 0, stats(0, 1), 0),
            FetchProgress::Advanced
        );
        assert_eq!(
            classify_progress(0, 0, stats(0, 0), 1),
            FetchProgress::Advanced
        );
    }
}
