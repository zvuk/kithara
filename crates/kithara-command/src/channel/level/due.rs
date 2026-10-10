use std::mem;

use super::{super::sender::Sent, LevelInbox};
use crate::{Batch, Outcome, Protocol, Receipt, Rejection, Seq, When};

/// A batch due inside the block whose basis matches the level's ledger.
/// ```compile_fail
/// # use kithara_command::{Inbox, Protocol};
/// fn take_two<P: Protocol>(inbox: &mut Inbox<P>, at: P::Clock) {
///     let first = inbox.next_due(at, 64);
///     let second = inbox.next_due(at, 64);
///     drop((first, second));
/// }
/// ```
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, get_mut)]
#[must_use = "answer through Due::apply, Due::refuse, Due::defer or Due::commit"]
pub struct Due<'inbox, P: Protocol> {
    pub(in crate::channel) inbox: LevelInbox<'inbox, P>,
    #[field(get(copy))]
    pub(in crate::channel) at: P::Clock,
    pub(in crate::channel) outcome: Option<Outcome<P>>,
    #[field(get(copy))]
    pub(in crate::channel) seq: Seq,
    #[field(get)]
    pub(in crate::channel) basis: Vec<(P::Target, Option<Seq>)>,
    #[field(get, get_mut(deref = false))]
    pub(in crate::channel) commands: Vec<P::Command>,
    #[field(get)]
    pub(in crate::channel) offset: usize,
}

impl<P: Protocol> Due<'_, P> {
    /// Records the batch and eagerly answers outdated batches now, retaining this batch
    /// and its credit until the executor completes it or the level returns its leftovers.
    pub fn commit(mut self) -> Seq {
        self.inbox.docket.ledger.record(&self.basis, self.seq);
        debug_assert!(
            self.inbox.docket.committed.len() < self.inbox.docket.committed.capacity(),
            "credits bound committed batches"
        );
        self.outcome = None;
        self.inbox.docket.committed.push((
            self.at,
            Sent {
                seq: self.seq,
                when: When::At(self.at),
                batch: Batch {
                    basis: mem::take(&mut self.basis),
                    commands: mem::take(&mut self.commands),
                },
            },
        ));
        self.inbox.reject_outdated();
        self.seq
    }

    /// Records every basis target and eagerly answers outdated batches of this level.
    /// ```compile_fail
    /// # use kithara_command::{Due, Protocol};
    /// fn move_basis<P: Protocol>(due: &mut Due<'_, P>) {
    ///     due.basis_mut()[0].1 = None;
    /// }
    /// ```
    pub fn apply(mut self, data: P::Applied) {
        self.inbox.docket.ledger.record(&self.basis, self.seq);
        self.outcome = Some(Outcome::Applied { data, at: self.at });
    }

    /// Answers the whole batch with the executor's refusal.
    pub fn refuse(mut self, refusal: P::Refusal) {
        self.outcome = Some(Outcome::Rejected(Rejection::Refused(refusal)));
    }

    /// Parks the judged batch, retaining its credit until it is resumed or retired.
    pub fn defer(mut self) -> Seq {
        self.outcome = None;
        self.inbox.docket.park(Sent {
            seq: self.seq,
            when: When::Deferred,
            batch: Batch {
                basis: mem::take(&mut self.basis),
                commands: mem::take(&mut self.commands),
            },
        });
        self.seq
    }
}

impl<P: Protocol> Drop for Due<'_, P> {
    fn drop(&mut self) {
        let Some(outcome) = self.outcome.take() else {
            return;
        };
        let applied = matches!(outcome, Outcome::Applied { .. });
        self.inbox.reply(Receipt {
            seq: self.seq,
            outcome,
            batch: Batch {
                basis: mem::take(&mut self.basis),
                commands: mem::take(&mut self.commands),
            },
        });
        if applied {
            self.inbox.reject_outdated();
        }
    }
}
