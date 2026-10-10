use crate::protocol::{Batch, Protocol, Seq};

/// Why a batch did not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection<R> {
    /// Its moment passed before the executor reached it.
    Late,
    /// Another batch shifted a target of its basis first.
    Stale,
    /// The executor refused it for a reason of its own domain.
    Refused(R),
    /// The executor dropped it once due without answering it, or dropped its
    /// inbox with the batch still in it.
    Unanswered,
}

/// What became of a batch.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<P: Protocol> {
    /// The whole batch applied.
    Applied {
        /// Frame the batch applied at.
        at: P::Clock,
        /// What the executor reports about it.
        data: P::Applied,
    },
    /// No command of the batch applied.
    Rejected(Rejection<P::Refusal>),
}

/// Answer to one sent batch, carrying the batch back.
///
/// The batch returns whatever the outcome, with anything the executor released
/// into it, so it is dropped on the sender's thread.
#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Receipt<P: Protocol> {
    /// The batch, with anything the executor released into it.
    #[field(get)]
    pub(crate) batch: Batch<P>,
    /// What became of the batch.
    #[field(get)]
    pub(crate) outcome: Outcome<P>,
    /// Number of the batch this receipt answers.
    #[field(get(copy))]
    pub(crate) seq: Seq,
}

impl<P: Protocol> From<Receipt<P>> for (Outcome<P>, Batch<P>) {
    /// Splits a receipt into the outcome and the returned batch.
    fn from(receipt: Receipt<P>) -> Self {
        (receipt.outcome, receipt.batch)
    }
}
