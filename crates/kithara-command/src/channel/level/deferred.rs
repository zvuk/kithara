use std::mem;

use super::{super::sender::Sent, LevelInbox};
use crate::{Batch, Outcome, Protocol, Receipt, Rejection, Seq, When};

/// A deferred arrival, not judged until it is parked or resumed.
#[must_use = "park or refuse the deferred batch"]
pub struct Deferred<'inbox, P: Protocol> {
    pub(in crate::channel) inbox: LevelInbox<'inbox, P>,
    pub(in crate::channel) seq: Seq,
    pub(in crate::channel) basis: Vec<(P::Target, Option<Seq>)>,
    pub(in crate::channel) commands: Vec<P::Command>,
    pub(in crate::channel) outcome: Option<Outcome<P>>,
}

impl<P: Protocol> Deferred<'_, P> {
    /// The batch's send number.
    #[must_use]
    pub fn seq(&self) -> Seq {
        self.seq
    }

    /// The batch's declared basis, not yet judged.
    #[must_use]
    pub fn basis(&self) -> &[(P::Target, Option<Seq>)] {
        &self.basis
    }

    /// Commands the executor may inspect before parking.
    #[must_use]
    pub fn commands(&self) -> &[P::Command] {
        &self.commands
    }

    /// Parks unless the basis is already outdated, in which case it answers Stale.
    pub fn park(mut self) -> Option<Seq> {
        if self.inbox.docket.ledger.outdates(&self.basis) {
            self.outcome = Some(Outcome::Rejected(Rejection::Stale));
            return None;
        }
        self.outcome = None;
        self.inbox.docket.park(Sent {
            seq: self.seq,
            when: When::Deferred,
            batch: Batch {
                basis: mem::take(&mut self.basis),
                commands: mem::take(&mut self.commands),
            },
        });
        Some(self.seq)
    }

    /// Refuses without parking or shifting any target.
    pub fn refuse(mut self, refusal: P::Refusal) {
        self.outcome = Some(Outcome::Rejected(Rejection::Refused(refusal)));
    }
}

impl<P: Protocol> Drop for Deferred<'_, P> {
    fn drop(&mut self) {
        let Some(outcome) = self.outcome.take() else {
            return;
        };
        self.inbox.reply(Receipt {
            seq: self.seq,
            outcome,
            batch: Batch {
                basis: mem::take(&mut self.basis),
                commands: mem::take(&mut self.commands),
            },
        });
    }
}
