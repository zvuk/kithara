use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    mem::take,
    sync::atomic::Ordering,
    task::Poll,
};

use kithara_abr::AbrPeerId;
use kithara_events::{DownloaderEvent, EventBus, RequestId, RequestPriority};
use kithara_platform::{
    CancelGroup, CancelToken,
    sync::{Arc, Notify, RwLock},
    time::{Instant, sleep},
    tokio,
    tokio::sync::mpsc,
};
use kithara_test_utils::kithara;
use thunderdome::{Arena, Index};
use tracing::trace;

use super::{
    batch::BatchGroup,
    downloader::{DownloaderInner, RegisteredPeerEntry},
    peer::{InternalCmd, Peer, ResponseTarget, SlotEntry},
};

/// Push a fetch command onto its priority slot — the moment a request
/// becomes eligible for dispatch. Wakes the urgent slot (`High`/`High`
/// or `High`/`Low`) consumer so it doesn't sit on the queue until the
/// next periodic poll, and fans the descriptor out to bus subscribers.
#[kithara::probe(request_id, priority)]
fn enqueue_request(
    slots: &mut [VecDeque<SlotEntry>; SLOT_COUNT],
    slot: usize,
    urgent_notify: &Notify,
    entry: SlotEntry,
    request_id: RequestId,
    priority: RequestPriority,
) {
    let bus = entry.cmd.bus.clone();
    let url = entry.cmd.cmd.url.clone();
    let method = entry.cmd.cmd.method;

    slots[slot].push_back(entry);

    if slot <= 1 {
        urgent_notify.notify_one();
    }

    if let Some(b) = bus {
        b.publish(DownloaderEvent::RequestEnqueued {
            request_id,
            url,
            method,
            priority,
        });
    }
}

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
    /// peer yielded a batch, an ABR tick ran, an in-flight fetch
    /// completed (inflight decremented), or a new fetch was dispatched.
    Advanced,
    /// Pending work exists (inflight > 0 or slots non-empty) but this
    /// tick observed no forward motion. Consecutive stalls across the
    /// watchdog window trigger a deadlock panic.
    Stalled,
}

/// Activity accumulated by [`Registry::tick`] so the downloader can classify
/// real forward motion rather than a spurious `poll_fn` wake.
#[derive(Default, Clone, Copy)]
struct PollStats {
    abr_ticked: bool,
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
const fn slot_index(peer_prio: RequestPriority, cmd_prio: RequestPriority) -> usize {
    match (peer_prio, cmd_prio) {
        (RequestPriority::High, RequestPriority::High) => Slot::HighHigh as usize,
        (RequestPriority::High, RequestPriority::Low) => Slot::HighLow as usize,
        (RequestPriority::Low, RequestPriority::High) => Slot::LowHigh as usize,
        (RequestPriority::Low, RequestPriority::Low) => Slot::LowLow as usize,
    }
}

/// Per-peer entry in the registry.
struct PeerEntry {
    /// ABR peer id stamped on every proactively-scheduled `InternalCmd`
    /// so the Downloader can credit bandwidth samples when the fetch
    /// completes.
    peer_id: AbrPeerId,
    /// Same Arc as the owning [`PeerHandle`]'s bus. Read-snapshot on
    /// every proactive poll so `PeerHandle::with_bus` takes effect
    /// immediately.
    bus: Arc<RwLock<Option<EventBus>>>,
    peer: Arc<dyn Peer>,
    peer_cancel: CancelToken,
    cmd_rx: mpsc::Receiver<InternalCmd>,
    peer_done: bool,
}

/// Peer registry: owns peers, routes commands to priority slots,
/// and drives batch execution.
#[derive(Default)]
pub(super) struct Registry {
    urgent_notify: Arc<Notify>,
    peers: Arena<PeerEntry>,
    slots: [VecDeque<SlotEntry>; SLOT_COUNT],
}

impl Registry {
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

    fn has_slot_work(&self) -> bool {
        self.slots.iter().any(|s| !s.is_empty())
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

        inner.fetch_waker.register(cx.waker());

        for (idx, entry) in &mut self.peers {
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
                let request_id = entry_slot.cmd.request_id;
                let priority = entry_slot.cmd.priority;
                enqueue_request(
                    &mut self.slots,
                    slot,
                    &self.urgent_notify,
                    entry_slot,
                    request_id,
                    priority,
                );
                stats.drained_cmds += 1;
            }

            if entry.peer_done {
                continue;
            }

            match entry.peer.poll_next(cx) {
                Poll::Ready(Some(batch)) => {
                    let peer_prio = entry.peer.priority();
                    let bus = entry.bus.read().clone();
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
                        enqueue_request(
                            &mut self.slots,
                            slot,
                            &self.urgent_notify,
                            entry_slot,
                            request_id,
                            cmd_prio,
                        );
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

        for idx in to_remove {
            self.peers.remove(idx);
        }

        stats
    }

    fn requeue_pending(&mut self, pending: Vec<SlotEntry>) {
        for entry in pending.into_iter().rev() {
            let peer_priority = entry
                .cmd
                .peer
                .and_then(|peer| self.peers.get(peer))
                .map_or(RequestPriority::Low, |peer| peer.peer.priority());
            let slot = slot_index(peer_priority, entry.cmd.effective_priority());
            self.slots[slot].push_front(entry);
        }
    }

    /// Re-ask `peer.priority()` and the command's live demand probe for
    /// each queued command, and move it to the correct slot when either
    /// answer changed. An escalation goes to the *front* of its new slot:
    /// a reader now blocks on those bytes, and the point is overtaking the
    /// queue that starved it. A demotion goes to the back.
    ///
    /// Each slot is rebuilt in one pass rather than patched by index: a
    /// slot can be both a source and a destination in the same pass, and
    /// a `push_front` shifts every index recorded before it.
    pub(super) fn reschedule(&mut self) {
        let mut escalated: Vec<(usize, SlotEntry)> = Vec::new();
        let mut demoted: Vec<(usize, SlotEntry)> = Vec::new();

        for slot_idx in 0..SLOT_COUNT {
            for slot_entry in take(&mut self.slots[slot_idx]) {
                let cmd = &slot_entry.cmd;
                let Some(peer_idx) = cmd.peer else {
                    self.slots[slot_idx].push_back(slot_entry);
                    continue;
                };
                let Some(entry) = self.peers.get(peer_idx) else {
                    let SlotEntry { cmd, peer_cancel } = slot_entry;
                    super::batch::deliver_cancelled_with_event(cmd, &peer_cancel);
                    continue;
                };
                let correct_slot = slot_index(entry.peer.priority(), cmd.effective_priority());
                if correct_slot == slot_idx {
                    self.slots[slot_idx].push_back(slot_entry);
                } else if correct_slot < slot_idx {
                    trace!(
                        request_id = ?cmd.request_id,
                        url = %cmd.cmd.url,
                        from_slot = slot_idx,
                        to_slot = correct_slot,
                        demanded = cmd.cmd.is_demanded(),
                        "reschedule: queued fetch escalated"
                    );
                    escalated.push((correct_slot, slot_entry));
                } else {
                    demoted.push((correct_slot, slot_entry));
                }
            }
        }

        for (slot, slot_entry) in demoted {
            self.slots[slot].push_back(slot_entry);
        }
        // WHY: Reverse keeps scan order among entries escalated into one slot.
        for (slot, slot_entry) in escalated.into_iter().rev() {
            self.slots[slot].push_front(slot_entry);
            if slot <= 1 {
                self.urgent_notify.notify_one();
            }
        }
    }

    /// Single tick: poll peers, process urgent, then demand with throttle.
    ///
    /// New peer registrations, queued ABR ticks, and the nearest ABR deadline
    /// are handled inside `poll_fn` rather than competing `select!` arms. This
    /// guarantees that `process()` runs to completion: no readiness source can
    /// drop `tick()` mid-batch and lose unspawned `FetchCmd`s.
    ///
    /// The deadline future completes once while the closure runs on every
    /// wakeup of this tick, so the elapsed flag latches rather than re-polling
    /// it. A deadline the controller then declines to tick on — its peer was
    /// cancelled while the loop slept — leaves the loop parked with that future
    /// finished, and resuming it panics the downloader out from under every
    /// peer it serves.
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
        let abr_deadline = inner.abr.next_tick_deadline();
        let abr_deadline_wait = async move {
            let Some(deadline) = abr_deadline else {
                return pending::<()>().await;
            };
            sleep(deadline.saturating_duration_since(Instant::now())).await;
        };
        tokio::pin!(abr_deadline_wait);
        let mut deadline_elapsed = false;

        poll_fn(|cx| {
            if !deadline_elapsed {
                deadline_elapsed = abr_deadline_wait.as_mut().poll(cx).is_ready();
            }
            aggregate.abr_ticked |= inner.abr.poll_ticks(cx, Instant::now(), deadline_elapsed);
            while let Poll::Ready(Some(entry)) = register_rx.poll_recv(cx) {
                self.add(entry);
            }
            let stats = self.poll_peers(cx, inner);
            aggregate.drained_cmds += stats.drained_cmds;
            aggregate.peer_batches += stats.peer_batches;
            if deadline_elapsed || aggregate.abr_ticked || self.has_slot_work() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;

        if aggregate.abr_ticked && !self.has_slot_work() {
            return FetchProgress::Advanced;
        }

        let mut dispatched: usize = 0;

        let urgent_batch = {
            let [s0, s1, _, _] = &mut self.slots;
            BatchGroup::from_iter(s0.drain(..).chain(s1.drain(..)))
        };
        if !urgent_batch.is_empty() {
            let result = urgent_batch.process(inner);
            dispatched += result.dispatched;
            let capacity_blocked = !result.pending.is_empty()
                && inner.inflight.load(Ordering::Relaxed) >= inner.max_concurrent;
            self.requeue_pending(result.pending);
            if capacity_blocked {
                inner.capacity_notify.notified().await;
            }
            return classify_progress(
                inflight_enter,
                inner.inflight.load(Ordering::Relaxed),
                aggregate,
                dispatched,
            );
        }

        if !inner.demand_throttle.is_zero() {
            let preempted_by_urgent = tokio::select! {
                () = sleep(inner.demand_throttle) => false,
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

        let demand_batch = {
            let [_, _, low_high, low_low] = &mut self.slots;
            BatchGroup::from_iter(low_high.drain(..).chain(low_low.drain(..)))
        };
        if !demand_batch.is_empty() {
            let result = demand_batch.process(inner);
            dispatched += result.dispatched;
            let capacity_blocked = !result.pending.is_empty()
                && inner.inflight.load(Ordering::Relaxed) >= inner.max_concurrent;
            self.requeue_pending(result.pending);
            if capacity_blocked {
                inner.capacity_notify.notified().await;
            }
        }

        classify_progress(
            inflight_enter,
            inner.inflight.load(Ordering::Relaxed),
            aggregate,
            dispatched,
        )
    }
}

/// A queued command still owns its peer's claim — the HLS segment slot and the
/// single non-`Clone` `AssetWriter` both ride in its `on_complete`. Dropping the
/// registry (downloader cancel leaves `Downloader::run`) would drop those
/// closures uncalled and strand the claim, so teardown delivers the
/// cancellation every queued command was still waiting for.
///
/// Peer channels need no such drain: they carry only imperative
/// `ResponseTarget::Channel` commands, whose caller already learns of the
/// cancellation from the dropped oneshot.
impl Drop for Registry {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            for SlotEntry { cmd, peer_cancel } in slot.drain(..) {
                super::batch::deliver_cancelled_with_event(cmd, &peer_cancel);
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
const fn classify_progress(
    inflight_enter: usize,
    inflight_exit: usize,
    poll_stats: PollStats,
    dispatched: usize,
) -> FetchProgress {
    let advanced = poll_stats.drained_cmds > 0
        || poll_stats.abr_ticked
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
    use kithara_test_utils::kithara;

    use super::{FetchProgress, PollStats, classify_progress};

    fn stats(drained: usize, batches: usize) -> PollStats {
        PollStats {
            abr_ticked: false,
            drained_cmds: drained,
            peer_batches: batches,
        }
    }

    #[kithara::test]
    fn advanced_when_abr_tick_ran() {
        let out = classify_progress(
            0,
            0,
            PollStats {
                abr_ticked: true,
                drained_cmds: 0,
                peer_batches: 0,
            },
            0,
        );
        assert_eq!(out, FetchProgress::Advanced);
    }

    #[kithara::test]
    #[case(0, 0)]
    fn idle_when_no_work_anywhere(#[case] inflight_enter: usize, #[case] inflight_exit: usize) {
        let out = classify_progress(inflight_enter, inflight_exit, stats(0, 0), 0);
        assert_eq!(out, FetchProgress::Idle);
    }

    #[kithara::test]
    #[case(1, 0, 0)]
    #[case(0, 1, 0)]
    #[case(0, 0, 1)]
    #[case(3, 0, 0)]
    #[case(0, 2, 0)]
    #[case(0, 0, 5)]
    #[case(2, 3, 4)]
    fn advanced_when_any_activity_counter_positive(
        #[case] drained: usize,
        #[case] batches: usize,
        #[case] dispatched: usize,
    ) {
        let out = classify_progress(0, 0, stats(drained, batches), dispatched);
        assert_eq!(out, FetchProgress::Advanced);
    }

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

    #[kithara::test]
    #[case(1, 1)]
    #[case(5, 5)]
    #[case(0, 3)]
    fn stalled_when_inflight_stuck_and_no_counters(
        #[case] inflight_enter: usize,
        #[case] inflight_exit: usize,
    ) {
        let out = classify_progress(inflight_enter, inflight_exit, stats(0, 0), 0);
        assert_eq!(out, FetchProgress::Stalled);
    }

    #[kithara::test]
    #[case(5, 5, 1, 0, 0)]
    #[case(5, 5, 0, 1, 0)]
    #[case(5, 5, 0, 0, 1)]
    #[case(5, 6, 0, 0, 1)]
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

    #[kithara::test]
    fn inflight_decrement_alone_yields_advanced() {
        assert_eq!(
            classify_progress(3, 2, stats(0, 0), 0),
            FetchProgress::Advanced
        );
    }
}
