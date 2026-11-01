use std::{collections::VecDeque, future::Future, mem::take, ops::Range};

use kithara_platform::sync::Notify;
use kithara_test_utils::kithara;
use thunderdome::Index;
use tracing::trace;

use crate::{DownloaderEvent, RequestPriority, batch::BatchGroup, consts, peer::SlotEntry};

#[repr(usize)]
enum Slot {
    HighHigh = 0,
    HighLow = 1,
    LowHigh = 2,
    LowLow = 3,
}

/// Map (`peer_priority`, `cmd_priority`) → slot index.
/// Processing order: 0 → 1 → 2 → 3.
pub(super) const fn slot_index(peer_prio: RequestPriority, cmd_prio: RequestPriority) -> usize {
    match (peer_prio, cmd_prio) {
        (RequestPriority::High, RequestPriority::High) => Slot::HighHigh as usize,
        (RequestPriority::High, RequestPriority::Low) => Slot::HighLow as usize,
        (RequestPriority::Low, RequestPriority::High) => Slot::LowHigh as usize,
        (RequestPriority::Low, RequestPriority::Low) => Slot::LowLow as usize,
    }
}

/// Owns queued commands, their priority order and urgent wakeups.
#[derive(Default)]
pub(super) struct Slots {
    urgent_notify: Notify,
    slots: [VecDeque<SlotEntry>; consts::SLOT_COUNT],
}

impl Slots {
    pub(super) fn has_work(&self) -> bool {
        self.slots.iter().any(|slot| !slot.is_empty())
    }

    delegate::delegate! {
        to self.urgent_notify {
            #[call(notify_one)]
            pub(super) fn notify_urgent(&self);
            #[call(notified)]
            pub(super) fn urgent_notified(&self) -> impl Future<Output = ()> + '_;
        }
    }

    pub(super) fn take(&mut self, range: Range<usize>) -> BatchGroup {
        BatchGroup::from_iter(self.slots[range].iter_mut().flat_map(|slot| slot.drain(..)))
    }

    pub(super) fn cancel_all(&mut self) {
        for slot in &mut self.slots {
            for SlotEntry { cmd, peer_cancel } in slot.drain(..) {
                crate::batch::deliver_cancelled_with_event(cmd, &peer_cancel);
            }
        }
    }

    /// Push a fetch command onto its priority slot — the moment a request
    /// becomes eligible for dispatch. Wakes the urgent slot (`High`/`High`
    /// or `High`/`Low`) consumer so it doesn't sit on the queue until the
    /// next periodic poll, and fans the descriptor out to bus subscribers.
    #[kithara::probe(request_id = entry.cmd.request_id, priority = entry.cmd.priority)]
    pub(super) fn enqueue(&mut self, slot: usize, entry: SlotEntry) {
        let request_id = entry.cmd.request_id;
        let priority = entry.cmd.priority;
        let bus = entry.cmd.bus.clone();
        let url = entry.cmd.cmd.url.clone();
        let method = entry.cmd.cmd.method;

        self.slots[slot].push_back(entry);

        if slot <= 1 {
            self.urgent_notify.notify_one();
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

    pub(super) fn requeue_pending(
        &mut self,
        pending: Vec<SlotEntry>,
        peer_priority: impl Fn(Index) -> Option<RequestPriority>,
    ) {
        for entry in pending.into_iter().rev() {
            let peer_priority = entry
                .cmd
                .peer
                .and_then(&peer_priority)
                .unwrap_or(RequestPriority::Low);
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
    pub(super) fn reschedule(&mut self, peer_priority: impl Fn(Index) -> Option<RequestPriority>) {
        let mut escalated: Vec<(usize, SlotEntry)> = Vec::new();
        let mut demoted: Vec<(usize, SlotEntry)> = Vec::new();

        for slot_idx in 0..consts::SLOT_COUNT {
            for slot_entry in take(&mut self.slots[slot_idx]) {
                let cmd = &slot_entry.cmd;
                if cmd.cancel.is_cancelled() {
                    let SlotEntry { cmd, peer_cancel } = slot_entry;
                    crate::batch::deliver_cancelled_with_event(cmd, &peer_cancel);
                    continue;
                }
                let Some(peer_idx) = cmd.peer else {
                    self.slots[slot_idx].push_back(slot_entry);
                    continue;
                };
                let Some(priority) = peer_priority(peer_idx) else {
                    let SlotEntry { cmd, peer_cancel } = slot_entry;
                    crate::batch::deliver_cancelled_with_event(cmd, &peer_cancel);
                    continue;
                };
                let correct_slot = slot_index(priority, cmd.effective_priority());
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
        for (slot, slot_entry) in escalated.into_iter().rev() {
            self.slots[slot].push_front(slot_entry);
            if slot <= 1 {
                self.urgent_notify.notify_one();
            }
        }
    }
}
