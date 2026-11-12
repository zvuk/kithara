use std::{collections::VecDeque, num::NonZeroUsize};

/// Frames rendered ahead of the reader's read mark.
///
/// The lead is bounded in frames, not packets: a short packet ending a source
/// revision takes only its own frames, so the next packet still fits beside it.
/// A reader spends its packets in order, so once it takes a packet every
/// packet it took before is spent, whether or not it ever hands that one back.
/// The latest packet it took is spent once every packet taken has come back,
/// and otherwise has at least one frame behind the read mark, so the lead past
/// that mark never reaches `(chunks + 1)` quanta. The ring has `chunks + 1`
/// slots: the frame bound queues at most `chunks` whole quanta, which leaves
/// one slot for the short packet a boundary cuts in front of them, and a source
/// whose packets stay below the quantum queues as many packets as before the
/// lead was counted in frames, plus that one. Packet weights are kept from
/// admission, since a reader may trim or keep a packet it takes.
pub(in crate::worker) struct PcmLead {
    limit: usize,
    quantum: usize,
    quanta: usize,
    frames: usize,
    weights: VecDeque<usize>,
    admitted: usize,
    returned: usize,
}

impl PcmLead {
    /// `chunks` quanta queued behind the packet the reader holds.
    pub(in crate::worker) fn new(chunks: NonZeroUsize, quantum: NonZeroUsize) -> Self {
        let quanta = chunks.get().saturating_add(1);
        Self {
            limit: quanta.saturating_mul(quantum.get()).saturating_sub(1),
            quantum: quantum.get(),
            quanta,
            frames: 0,
            weights: VecDeque::with_capacity(quanta.saturating_add(2)),
            admitted: 0,
            returned: 0,
        }
    }

    /// Forward ring slots: one per quantum of lead.
    pub(in crate::worker) const fn slots(&self) -> usize {
        self.quanta
    }

    /// The lead while `queued` packets still wait in the forward ring.
    fn lead(&self, queued: usize) -> usize {
        let taken = self.weights.len().saturating_sub(queued);
        let Some(latest) = taken.checked_sub(1) else {
            return self.frames;
        };
        let spent: usize = self.weights.range(..latest).sum();
        let held = self.admitted.saturating_sub(queued) > self.returned;
        let behind = if held { 1 } else { self.weights[latest] };
        self.frames - spent - behind
    }

    /// Whether `frames` more fit the lead while `queued` packets still wait in
    /// the forward ring.
    pub(super) fn admits(&self, frames: usize, queued: usize) -> bool {
        let lead = self.lead(queued);
        lead == 0 || lead.saturating_add(frames) <= self.limit
    }

    /// Admit `frames` once every packet taken before the latest one is spent.
    pub(super) fn add(&mut self, frames: usize, queued: usize) {
        let taken = self.weights.len().saturating_sub(queued);
        for spent in self.weights.drain(..taken.saturating_sub(1)) {
            self.frames -= spent;
        }
        self.frames += frames;
        self.weights.push_back(frames);
        self.admitted += 1;
    }

    /// Whether a whole quantum more fits the lead.
    pub(super) fn admits_quantum(&self, queued: usize) -> bool {
        self.admits(self.quantum, queued)
    }

    /// Count one packet the reader handed back.
    pub(super) const fn count_return(&mut self) {
        self.returned += 1;
    }

    /// The lead holds more than one quantum behind the reader's packet.
    pub(in crate::worker) const fn queues_ahead(&self) -> bool {
        self.limit > self.quantum.saturating_mul(2)
    }
}
