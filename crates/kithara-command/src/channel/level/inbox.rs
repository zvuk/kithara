use futures::task::AtomicWaker;

use super::{
    super::{docket::Docket, scoped::Lifecycle, sender::Sent, sink::Sink},
    Deferred, Due,
};
use crate::{Outcome, Protocol, Receipt, Rejection, Seq, When};

/// An executor's view of one command level, with its own ledger and capacity.
pub struct LevelInbox<'inbox, P: Protocol> {
    pub(in crate::channel) docket: &'inbox mut Docket<P>,
    pub(in crate::channel) sink: Sink<'inbox, P>,
    pub(in crate::channel) answered: &'inbox AtomicWaker,
    pub(in crate::channel) lifecycle: Option<&'inbox mut Lifecycle>,
}

impl<'inbox, P: Protocol> LevelInbox<'inbox, P> {
    pub(super) fn reborrow(&mut self) -> LevelInbox<'_, P> {
        LevelInbox {
            docket: self.docket,
            sink: self.sink.reborrow(),
            answered: self.answered,
            lifecycle: self.lifecycle.as_deref_mut(),
        }
    }

    delegate::delegate! {
        to self.docket {
            /// Frames until the earliest Next or At batch; deferred and parked batches are excluded.
            #[must_use]
            pub fn frames_until_due(&self, start: P::Clock) -> Option<u64>;
            /// Whether this level still holds the parked batch.
            #[must_use]
            pub fn is_parked(&self, seq: Seq) -> bool;
            /// Edits a committed batch in place without taking it out of this level.
            pub fn committed_mut(&mut self, seq: Seq) -> Option<&mut [P::Command]>;
        }
    }

    /// Yields the next due batch, answering Late or Stale batches along the way.
    pub fn next_due(&mut self, start: P::Clock, frames: usize) -> Option<Due<'_, P>> {
        self.reborrow().take_due(start, frames)
    }

    pub(in crate::channel) fn take_due(
        mut self,
        start: P::Clock,
        frames: usize,
    ) -> Option<Due<'inbox, P>> {
        if frames == 0 {
            return None;
        }
        loop {
            let (at, offset) = match self.docket.schedule.peek()? {
                When::Next => (start, Some(Ok(0))),
                When::At(at) => (at, P::frames_since(at, start).map(usize::try_from)),
                When::Deferred => return None,
            };
            let offset = match offset {
                Some(Ok(offset)) if offset < frames => Some(offset),
                Some(_) => return None,
                None => None,
            };
            let sent = self.docket.schedule.pop()?;
            let rejection = match offset {
                Some(offset) if self.docket.ledger.is_current(&sent.batch.basis) => {
                    return Some(self.due(sent, at, offset));
                }
                Some(_) => Rejection::Stale,
                None => Rejection::Late,
            };
            self.reject(sent, rejection);
        }
    }

    fn due(self, sent: Sent<P>, at: P::Clock, offset: usize) -> Due<'inbox, P> {
        Due {
            inbox: self,
            at,
            offset,
            seq: sent.seq,
            basis: sent.batch.basis,
            commands: sent.batch.commands,
            outcome: Some(Outcome::Rejected(Rejection::Unanswered)),
        }
    }

    /// Takes an arrival without judging its basis.
    pub fn next_deferred(&mut self) -> Option<Deferred<'_, P>> {
        self.reborrow().take_deferred()
    }

    pub(in crate::channel) fn take_deferred(self) -> Option<Deferred<'inbox, P>> {
        if self.docket.arrived.is_empty() {
            return None;
        }
        let sent = self.docket.arrived.remove(0);
        Some(Deferred {
            inbox: self,
            seq: sent.seq,
            basis: sent.batch.basis,
            commands: sent.batch.commands,
            outcome: Some(Outcome::Rejected(Rejection::Unanswered)),
        })
    }

    /// Resumes a parked batch at `at`, judging it again and measuring its offset from `start`.
    pub fn resume(&mut self, seq: Seq, start: P::Clock, at: P::Clock) -> Option<Due<'_, P>> {
        self.reborrow().take_parked(seq, start, at)
    }

    pub(in crate::channel) fn take_parked(
        mut self,
        seq: Seq,
        start: P::Clock,
        at: P::Clock,
    ) -> Option<Due<'inbox, P>> {
        let index = self.docket.parked.iter().position(|sent| sent.seq == seq)?;
        let offset = P::frames_since(at, start).and_then(|frames| usize::try_from(frames).ok());
        debug_assert!(
            offset.is_some(),
            "the firing moment must be inside the block"
        );
        let offset = offset?;
        let sent = self.docket.parked.swap_remove(index);
        if !self.docket.ledger.is_current(&sent.batch.basis) {
            self.reject(sent, Rejection::Stale);
            return None;
        }
        Some(self.due(sent, at, offset))
    }

    /// Answers a committed batch at its original moment, without judging its basis again.
    /// Returns false if this level no longer holds the committed batch.
    pub fn complete(&mut self, seq: Seq, data: P::Applied) -> bool {
        let Some(index) = self
            .docket
            .committed
            .iter()
            .position(|(_, sent)| sent.seq == seq)
        else {
            return false;
        };
        let (at, sent) = self.docket.committed.swap_remove(index);
        self.reply(Receipt {
            seq: sent.seq,
            batch: sent.batch,
            outcome: Outcome::Applied { at, data },
        });
        true
    }

    /// Whether this scope's Close has been drained; always false for the root.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.lifecycle
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.closing)
    }

    /// Returns leftovers whole, clears the ledger, advances generation and answers Closed last.
    ///
    /// # Panics
    /// Debug builds assert that only a closing scope is retired.
    pub fn retire(mut self) {
        debug_assert!(self.is_closing(), "only a closing scope can retire");
        if !self.is_closing() {
            return;
        }
        self.unanswered();
        self.docket.ledger.reset();
        if let Some(lifecycle) = self.lifecycle {
            lifecycle.generation = lifecycle.generation.wrapping_add(1);
            lifecycle.closing = false;
        }
        self.sink.closed(self.answered);
    }

    pub(in crate::channel) fn refuse_timed(&mut self, refusal: P::Refusal)
    where
        P::Refusal: Clone,
    {
        while let Some(sent) = self.docket.schedule.take_timed() {
            self.reject(sent, Rejection::Refused(refusal.clone()));
        }
    }

    pub(in crate::channel) fn unanswered(&mut self) {
        while let Some((_, sent)) = self.docket.committed.pop() {
            self.reject(sent, Rejection::Unanswered);
        }
        while let Some(sent) = self.docket.arrived.pop() {
            self.reject(sent, Rejection::Unanswered);
        }
        while let Some(sent) = self.docket.parked.pop() {
            self.reject(sent, Rejection::Unanswered);
        }
        while let Some(sent) = self.docket.schedule.pop() {
            self.reject(sent, Rejection::Unanswered);
        }
    }

    fn reject(&mut self, sent: Sent<P>, rejection: Rejection<P::Refusal>) {
        self.reply(Receipt {
            seq: sent.seq,
            batch: sent.batch,
            outcome: Outcome::Rejected(rejection),
        });
    }

    pub(super) fn reply(&mut self, receipt: Receipt<P>) {
        self.sink.reply(receipt, self.answered);
    }

    pub(super) fn reject_outdated(&mut self) {
        while let Some(sent) = self.docket.take_outdated() {
            self.reject(sent, Rejection::Stale);
        }
    }
}
