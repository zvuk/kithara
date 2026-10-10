use std::num::NonZeroUsize;

use kithara_platform::time::Duration;

/// Background loads a queue keeps open by default.
pub(crate) const DEFAULT_MAX_CONCURRENT_LOADS: NonZeroUsize = match NonZeroUsize::new(3) {
    Some(count) => count,
    None => unreachable!(),
};
/// Dispatcher batches a background load leaves free in one pass: a superseded
/// target's Release, an old replacement's Release, and a select's Load or `SetPriority`.
pub(crate) const SELECT_DISPATCH_RESERVE: usize = 3;

/// Default session time before a track ends at which the queue loads its
/// successor.
pub(crate) const DEFAULT_PRELOAD_LEAD: Duration = Duration::from_millis(3_500);
