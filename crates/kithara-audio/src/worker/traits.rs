//! Audio worker traits and effect utilities.

// `unimock::unimock` macro expands a generated `MockAudioWorkerSource` impl
// that matches over `()` returns with `_` patterns. The macro output isn't
// editable, so suppress the lint at the module level when test-utils are
// compiled in.
#![cfg_attr(
    any(test, feature = "test-utils"),
    allow(clippy::ignored_unit_patterns)
)]

use kithara_decode::PcmChunk;
use kithara_stream::Timeline;

use crate::{pipeline::track_fsm, traits::AudioEffect};

/// Trait for audio sources processed in a blocking worker thread.
///
/// The worker calls `step_track()` once per scheduling round. Each call
/// performs at most one FSM transition and returns a [`TrackStep`] that
/// tells the worker what happened.
#[cfg_attr(any(test, feature = "test-utils"), unimock::unimock(api = MockAudioWorkerSource, type Chunk = PcmChunk;))]
pub trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;

    /// Advance the track FSM by one step.
    ///
    /// Handles seek preemption, source readiness, decoding, and all
    /// internal state transitions. Returns:
    /// - `Produced` — decoded chunk ready for the consumer.
    /// - `StateChanged` — internal transition; caller should call again.
    /// - `Blocked` — the source is not ready; the caller should wait for a wake.
    /// - `Eof` — end of stream (may transition out via seek-after-EOF).
    /// - `Failed` — terminal failure.
    fn step_track(&mut self) -> track_fsm::TrackStep<Self::Chunk>;

    /// Access the shared timeline for epoch queries.
    fn timeline(&self) -> &Timeline;
}

/// Apply the effect chain to the chunk.
pub(crate) fn apply_effects(
    effects: &mut [Box<dyn AudioEffect>],
    mut chunk: PcmChunk,
) -> Option<PcmChunk> {
    for effect in &mut *effects {
        chunk = effect.process(chunk)?;
    }
    Some(chunk)
}

/// Flush effects chain at end of stream.
pub(crate) fn flush_effects(effects: &mut [Box<dyn AudioEffect>]) -> Option<PcmChunk> {
    let mut chunk: Option<PcmChunk> = None;
    for effect in &mut *effects {
        chunk = match chunk.take() {
            Some(input) => effect.process(input),
            None => effect.flush(),
        };
    }
    chunk
}

/// Reset effects chain (e.g. after seek).
pub(crate) fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in &mut *effects {
        effect.reset();
    }
}
