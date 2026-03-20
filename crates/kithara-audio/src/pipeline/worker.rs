//! Audio worker types, traits, and effects utilities.

use kithara_decode::PcmChunk;
use kithara_stream::Timeline;

use crate::{pipeline::track_fsm::TrackStep, traits::AudioEffect};

/// Command for audio worker (non-seek commands only).
///
/// Seek flows entirely through [`Timeline`] atomics.
#[derive(Debug)]
pub(crate) enum AudioCommand {
    // Seek removed — flows through Timeline::initiate_seek / complete_seek.
}

/// Trait for audio sources processed in a blocking worker thread.
///
/// The worker calls `step_track()` once per scheduling round. Each call
/// performs at most one FSM transition and returns a [`TrackStep`] that
/// tells the worker what happened.
pub(crate) trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    /// Advance the track FSM by one step.
    ///
    /// Handles seek preemption, source readiness, decoding, and all
    /// internal state transitions. Returns:
    /// - `Produced` — decoded chunk ready for the consumer.
    /// - `StateChanged` — internal transition; caller should call again.
    /// - `Blocked` — source not ready; caller should wait for a wake.
    /// - `Eof` — end of stream (may transition out via seek-after-EOF).
    /// - `Failed` — terminal failure.
    fn step_track(&mut self) -> TrackStep<Self::Chunk>;

    fn handle_command(&mut self, cmd: Self::Command);

    /// Access the shared timeline for epoch queries.
    fn timeline(&self) -> &Timeline;
}

/// Apply effects chain to a chunk.
pub(super) fn apply_effects(
    effects: &mut [Box<dyn AudioEffect>],
    mut chunk: PcmChunk,
) -> Option<PcmChunk> {
    for effect in &mut *effects {
        chunk = effect.process(chunk)?;
    }
    Some(chunk)
}

/// Flush effects chain at end of stream.
pub(super) fn flush_effects(effects: &mut [Box<dyn AudioEffect>]) -> Option<PcmChunk> {
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
pub(super) fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in &mut *effects {
        effect.reset();
    }
}
