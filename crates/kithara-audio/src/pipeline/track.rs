use crate::Fetch;

/// Result of one owner-thread decode step.
pub enum TrackStep<C> {
    Produced(Fetch<C>),
    Blocked(WaitingReason),
    StateChanged,
    Eof,
    Failed(crate::TrackFailureKind),
}

/// Why source progress is waiting on upstream work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitingReason {
    Waiting,
    WaitingDemand,
    WaitingMetadata,
}
