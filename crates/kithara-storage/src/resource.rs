#![forbid(unsafe_code)]

use std::ops::Range;

use rangemap::RangeSet;

/// Controls how [`MmapDriver`](crate::MmapDriver) opens the backing file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenMode {
    /// Auto-detect: existing files open as committed (read-only mmap),
    /// new files open as active (read-write mmap). Default behavior.
    #[default]
    Auto,
    /// Always read-write. Existing files are opened without truncation.
    /// Writes after commit transparently reopen the file as read-write.
    /// Use for files that are rewritten in place (e.g. index files).
    ReadWrite,
    /// Always read-only. Existing files are committed; missing files
    /// are treated as empty committed resources. Writes are rejected.
    ReadOnly,
}

/// Outcome of waiting for a byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The requested range is available for reading.
    Ready,
    /// The resource has been committed and the requested range starts at/after EOF.
    Eof,
    /// A seek or flush interrupted the wait. The caller should abort the current
    /// read and check for pending seeks.
    Interrupted,
}

/// Status of a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceStatus {
    /// Resource is open for writing (streaming in progress).
    Active,
    /// Resource has been committed (all data written).
    Committed { final_len: Option<u64> },
    /// Resource encountered an error.
    Failed(String),
    /// Resource's cancellation token has fired and the data
    /// lifecycle has not progressed past `Active` (no committed
    /// bytes, no recorded failure).
    ///
    /// `Committed { .. }` and `Failed(_)` retain priority because
    /// their data outcomes already classify the resource — observers
    /// that want to read the bytes a `Committed` resource produced
    /// before being cancelled need not be denied. Treat `Cancelled`
    /// as the routine shutdown signal that supersedes `Active`
    /// **only** when there is no other lifecycle classification to
    /// surface.
    Cancelled,
}

/// Check if the `available` range set fully covers `range`.
pub(crate) fn range_covered_by(available: &RangeSet<u64>, range: &Range<u64>) -> bool {
    if range.is_empty() {
        return true;
    }
    !available.gaps(range).any(|_| true)
}
