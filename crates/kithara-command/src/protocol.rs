use std::{convert::Infallible, fmt::Debug, num::NonZeroU64};

/// Number a channel gives a batch when it is sent.
///
/// Numbers start at one and grow in send order, so a receipt names the batch
/// it answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Seq(NonZeroU64);

impl Seq {
    pub(crate) const FIRST: Self = Self(NonZeroU64::MIN);

    /// The number as an integer, for logs and probes.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Moment a batch applies at, in the executor's clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum When<T> {
    /// The start of the first block the executor renders after the batch
    /// arrives.
    Next,
    /// This frame; a frame the executor already passed makes the batch late.
    At(T),
    /// An event in the executor's stream names the moment. Ordered after every timed batch.
    Deferred,
}

impl<T> Default for When<T> {
    /// The nearest moment: the next block.
    fn default() -> Self {
        Self::Next
    }
}

/// Something whose time a batch shifts, such as a deck slot or a transport.
pub trait Target: Copy {
    /// Position of this target among the targets a channel tracks.
    fn index(self) -> usize;
}

/// A protocol whose batches shift no time names no target.
impl Target for Infallible {
    fn index(self) -> usize {
        match self {}
    }
}

/// Types one executor speaks: its commands, targets, clock and answers.
///
/// Every type is [`Debug`] so batches, receipts and send errors print in logs
/// and test failures. The clock is any ordered frame counter; the protocol
/// counts frames on it, so the clock type needs no trait of this crate.
pub trait Protocol {
    /// What the executor reports about a batch it applied.
    type Applied: Debug;
    /// The executor's frame counter.
    type Clock: Copy + Ord + Debug;
    /// One command the executor applies.
    type Command: Debug;
    /// Why the executor refused a due batch.
    type Refusal: Debug;
    /// A target whose time a batch shifts.
    type Target: Target + Debug;

    /// Frames from `start` to `at`, or `None` when `at` comes before `start`.
    fn frames_since(at: Self::Clock, start: Self::Clock) -> Option<u64>;
}

/// Commands applied together, and the basis they were computed from.
///
/// The basis lists every target the batch shifts, each with the last batch
/// expected to have shifted it by the time this one applies, or `None` when
/// no batch should have. Parameters shift no time and carry an empty basis.
#[derive(Debug)]
pub struct Batch<P: Protocol> {
    /// Targets this batch shifts, each with the batch expected to shift it last.
    pub basis: Vec<(P::Target, Option<Seq>)>,
    /// Commands in the order the executor applies them.
    pub commands: Vec<P::Command>,
}
