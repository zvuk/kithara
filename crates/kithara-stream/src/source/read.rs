use std::num::NonZeroUsize;

/// Phase of a source's wait/read lifecycle.
///
/// Each `Source` implementation returns the current phase from its
/// `phase()` method — a point-in-time snapshot for external observers
/// (audio pipeline, tracing, UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SourcePhase {
    /// Cancelled — terminal, source will not produce more data.
    Cancelled,
    /// End of stream reached.
    Eof,
    /// Requested range is available for non-blocking read.
    Ready,
    /// Default: data not yet available, no specific sub-state.
    #[default]
    Waiting,
    /// On-demand request is queued or already in flight for this reader.
    WaitingDemand,
    /// Metadata lookup needed before data can be requested.
    WaitingMetadata,
}

/// Reason a [`ReadOutcome::Pending`] was returned — i.e. why the source
/// did not make progress this call. Each variant maps to a distinct
/// caller action; there is no overlap and no string-matching required.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[non_exhaustive]
#[derive(derive_more::Error)]
#[error(ignore)]
pub enum PendingReason {
    /// Data is not yet available at the requested range. Transient —
    /// caller may retry after backoff. The inner [`NotReadyCause`] tells
    /// which point in the read pipeline failed to make progress (wait
    /// budget exhausted, wait interrupted, source-side pending).
    #[display("data not ready ({_0})")]
    NotReady(NotReadyCause),
    /// Source crossed a variant boundary at this offset. Caller must
    /// recreate the decoder before reads succeed. Zero bytes were
    /// touched — the boundary surfaces BEFORE any data is read.
    #[display("variant change: decoder recreation required")]
    VariantChange,
    /// The session this read went through was retired: ownership moved
    /// to another session (a variant promotion committing, a prepared
    /// transition being discarded) or the stream itself was torn down.
    /// The bytes are not gone — this reader is. Caller must rebuild
    /// against whichever session the stream now owns; a teardown then
    /// surfaces as a cancelled source, not as a decode failure.
    #[display("session retired, rebuild against the current owner")]
    SessionRetired,
    /// Resource was evicted between [`crate::Source::wait_range`] (metadata
    /// ready) and [`crate::Source::read_at`] (actual I/O). Caller should
    /// retry from `wait_range`, not from the same byte offset.
    #[display("resource evicted, retry wait_range")]
    Retry,
}

/// Concrete cause for a [`PendingReason::NotReady`].
///
/// Carried as the typed payload of `NotReady` so the `io::Error` that
/// `impl Read for Stream` produces names the real stall site without
/// requiring decoder-side instrumentation.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotReadyCause {
    /// `wait_range` returned `WaitBudgetExceeded` for `MAX_WAIT_SPINS`
    /// iterations — the source kept signalling "not yet" past the read
    /// budget. Typical when a fetch is slower than the read deadline.
    #[display("wait budget exhausted")]
    WaitBudgetExhausted,
    /// `wait_range` returned `Interrupted` without a terminal interruption, also
    /// past the spin budget — the downloader woke us but range still
    /// wasn't satisfied. Typical sign of a flapping ABR/eviction race.
    #[display("wait interrupted, source wait")]
    WaitInterrupted,
    /// `wait_range` reported ready but `read_at` then returned `Pending`
    /// with a non-`Retry` reason — surfaced verbatim from the source.
    #[display("source returned pending after wait ready")]
    SourcePending,
}

/// Outcome of a [`crate::Source::read_at`] call.
///
/// Each variant has distinct caller semantics — there is no
/// overload of a numeric zero. `Bytes` carries a typed
/// [`NonZeroUsize`] so the type system guarantees forward progress;
/// `Pending` carries an explicit [`PendingReason`]; `Eof` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Source produced `count` bytes (`count > 0` by construction).
    Bytes(NonZeroUsize),
    /// Source did not make progress this call. See [`PendingReason`]
    /// for the precise cause and required caller action.
    Pending(PendingReason),
    /// Natural end of stream — no more bytes will ever come from this
    /// source at this offset.
    Eof,
}
