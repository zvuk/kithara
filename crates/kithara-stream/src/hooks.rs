//! Reader-side decoder hooks.
//!
//! `kithara-stream` defines the **shape** of the hook contract — what
//! the decoder layer signals, what arguments arrive — without depending
//! on `kithara-decode` or knowing about PCM payloads. The hook trait
//! takes lightweight signals (`ReaderChunkSignal` / `ReaderSeekSignal`)
//! that mirror the meaningful shape of `DecoderChunkOutcome` /
//! `DecoderSeekOutcome` for event-emission purposes only — no PCM
//! buffer, no decoder-specific error type.
//!
//! `kithara-decode` owns the wrapper (`HookedDecoder`) that intercepts
//! `InnerDecoder::next_chunk` / `seek` and feeds the corresponding
//! signal into the hook. Source impls (`HlsSource`, `FileSource`) live
//! in their own crates and produce hook instances via
//! [`Source::take_reader_hooks`](crate::Source::take_reader_hooks).

use std::sync::{Arc, Mutex};

use crate::source::PendingReason;

/// Lightweight read-side signal fed into [`DecoderHooks::on_chunk`].
///
/// Mirrors the meaningful shape of `DecoderChunkOutcome` (in
/// `kithara-decode`) without the PCM payload — hooks emit events based
/// on byte-cursor state from the [`Timeline`](crate::Timeline), not on
/// the raw audio frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReaderChunkSignal {
    /// Decoder produced a PCM chunk.
    Chunk,
    /// Decoder is alive but produced nothing this call (typed reason).
    Pending(PendingReason),
    /// Natural end of stream — no more chunks will arrive.
    Eof,
}

/// Lightweight seek-side signal fed into [`DecoderHooks::on_seek`].
///
/// Mirrors the meaningful shape of `DecoderSeekOutcome` (in
/// `kithara-decode`) for event-emission purposes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReaderSeekSignal {
    /// Decoder parked at the destination. `landed_byte` is the absolute
    /// byte offset the decoder picked (granule-aligned), when the
    /// backend exposes one.
    Landed { landed_byte: Option<u64> },
    /// Seek target was past the decoder's known duration. The decoder
    /// is now parked at EOF.
    PastEof,
}

/// Reader-side hooks invoked by the decoder layer's `HookedDecoder`
/// right before it forwards the inner decoder's typed outcome to the
/// caller.
///
/// One call per `next_chunk` / `seek` — granularity is decoder
/// operations, not byte-level reads.
pub trait DecoderHooks: Send + Sync {
    /// Called once per `next_chunk` after the inner decoder produced
    /// an outcome.
    fn on_chunk(&mut self, signal: ReaderChunkSignal);

    /// Called once per `seek` after the inner decoder parked at the
    /// destination (or signalled `PastEof`).
    fn on_seek(&mut self, signal: ReaderSeekSignal);
}

/// Shared, lockable hook handle. Used so that `Source::take_reader_hooks`
/// can hand off Clone-able ownership and the hook implementation can
/// hold `&mut self` state behind a single lock.
pub type SharedHooks = Arc<Mutex<dyn DecoderHooks>>;
