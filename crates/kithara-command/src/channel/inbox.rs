use std::{
    ops::Range,
    task::{Context, Poll},
};

use futures::task::AtomicWaker;
use kithara_platform::sync::Arc;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Observer},
};

use super::{
    docket::Docket,
    gate::Gate,
    level::{Deferred, Due, LevelInbox},
    sender::Sent,
    sink::Sink,
};
use crate::{ChannelConfig, Protocol, Receipt, Seq};

/// Receiving half of a one-level channel, owned by its executor.
pub struct Inbox<P: Protocol> {
    pending: HeapCons<Sent<P>>,
    answers: HeapProd<Receipt<P>>,
    wake: Arc<AtomicWaker>,
    answered: Arc<AtomicWaker>,
    gate: Arc<Gate>,
    docket: Docket<P>,
}

/// One stretch of a block an executor renders through [`Inbox::run_block`].
pub enum Step<'inbox, P: Protocol> {
    /// Frames uninterrupted by a due batch.
    Run(Range<usize>),
    /// A batch at the end of the preceding stretch.
    Due(Due<'inbox, P>),
}

impl<P: Protocol> Inbox<P> {
    pub(super) fn new(
        pending: HeapCons<Sent<P>>,
        answers: HeapProd<Receipt<P>>,
        wake: Arc<AtomicWaker>,
        answered: Arc<AtomicWaker>,
        gate: Arc<Gate>,
        config: ChannelConfig,
    ) -> Self {
        Self {
            pending,
            answers,
            wake,
            answered,
            gate,
            docket: Docket::new(config),
        }
    }

    fn level(&mut self) -> LevelInbox<'_, P> {
        LevelInbox {
            docket: &mut self.docket,
            sink: Sink::Plain(&mut self.answers),
            answered: &self.answered,
            lifecycle: None,
        }
    }

    /// Drains arrivals without judging them; Deferred batches bypass the timed schedule.
    pub fn drain(&mut self) {
        while let Some(sent) = self.pending.try_pop() {
            self.docket.insert(sent);
        }
    }

    /// Drains and waits for arrivals or sender teardown, waking through `cx`.
    pub fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.wake.register(cx.waker());
        let closed = self.is_closed();
        self.drain();
        if closed || self.docket.schedule.peek().is_some() || !self.docket.arrived.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Whether the sender is gone and no more batches can arrive.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        !self.pending.write_is_held()
    }

    delegate::delegate! {
        to self.docket {
            /// Frames until the earliest timed batch, excluding arrivals and parked batches.
            #[must_use]
            pub fn frames_until_due(&self, start: P::Clock) -> Option<u64>;
            /// Whether the batch remains parked in this level.
            #[must_use]
            pub fn is_parked(&self, seq: Seq) -> bool;
            /// Edits a committed batch in place while the inbox retains it and its credit.
            pub fn committed_mut(&mut self, seq: Seq) -> Option<&mut [P::Command]>;
        }
    }

    /// The next due batch, in time then send order, with Late and Stale answered on the way.
    pub fn next_due(&mut self, start: P::Clock, frames: usize) -> Option<Due<'_, P>> {
        self.level().take_due(start, frames)
    }

    /// A deferred arrival, with no verdict until the executor parks or refuses it.
    pub fn next_deferred(&mut self) -> Option<Deferred<'_, P>> {
        self.level().take_deferred()
    }

    /// Resumes a parked batch at `at`, judging its basis and measuring its offset from `start`.
    pub fn resume(&mut self, seq: Seq, start: P::Clock, at: P::Clock) -> Option<Due<'_, P>> {
        self.level().take_parked(seq, start, at)
    }

    /// Answers a committed batch at its original moment, without judging its basis again.
    /// Returns false if the inbox no longer holds the committed batch.
    pub fn complete(&mut self, seq: Seq, data: P::Applied) -> bool {
        self.level().complete(seq, data)
    }

    /// Drains and walks one block, handing each uninterrupted stretch and due batch to `step`.
    pub fn run_block<F: FnMut(Step<'_, P>)>(
        &mut self,
        start: P::Clock,
        frames: usize,
        mut step: F,
    ) {
        self.drain();
        let mut reached = 0;
        while let Some(due) = self.next_due(start, frames) {
            let offset = due.offset();
            if offset > reached {
                step(Step::Run(reached..offset));
                reached = offset;
            }
            step(Step::Due(due));
        }
        if reached < frames {
            step(Step::Run(reached..frames));
        }
    }

    /// Drains and refuses every At batch, leaving Next, Deferred and parked batches untouched.
    pub fn refuse_timed(&mut self, refusal: P::Refusal)
    where
        P::Refusal: Clone,
    {
        self.drain();
        self.level().refuse_timed(refusal);
    }
}

impl<P: Protocol> Drop for Inbox<P> {
    fn drop(&mut self) {
        self.gate.close();
        self.drain();
        self.level().unanswered();
    }
}
