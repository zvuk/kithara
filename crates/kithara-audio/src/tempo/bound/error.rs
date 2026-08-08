use kithara_stretch::ElasticError;

use crate::musical::ScheduleError;

/// A bound deck could not render the block the session grid asked for.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum BoundError {
    /// The schedule could not resolve the block's output frames.
    #[error("bound schedule: {0}")]
    Schedule(#[from] ScheduleError),
    /// The exact-span engine or planner rejected the block.
    #[error("bound engine: {0}")]
    Elastic(#[from] ElasticError),
    /// A block plan produced no segment, which its own constructor forbids.
    #[error("bound plan produced no segment")]
    EmptyPlan,
    /// A frame count or product left the representable range.
    #[error("bound block size is not representable")]
    BlockOverflow,
    /// The plan asked for source the slot has already consumed. The slot is
    /// forward-only and retains nothing, so this is a contract break, never a
    /// condition to clamp or re-seek around.
    #[error("bound plan needs source frame {requested} behind the retained start {available}")]
    BehindWindow { requested: i64, available: u64 },
}
