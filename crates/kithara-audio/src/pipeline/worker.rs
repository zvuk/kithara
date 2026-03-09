//! Audio worker types, traits, and effects utilities.

use kithara_decode::PcmChunk;
use kithara_stream::{Fetch, Timeline};

use crate::traits::AudioEffect;

/// Command for audio worker (non-seek commands only).
///
/// Seek flows entirely through [`Timeline`] atomics.
#[derive(Debug)]
pub(crate) enum AudioCommand {
    // Seek removed — flows through Timeline::initiate_seek / complete_seek.
}

/// Trait for audio sources processed in a blocking worker thread.
pub(crate) trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;
    type Command: Send + 'static;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk>;
    fn handle_command(&mut self, cmd: Self::Command);

    /// Access the shared timeline for flushing checks.
    fn timeline(&self) -> &Timeline;

    /// Apply a pending seek read from the Timeline.
    ///
    /// Called by the worker loop when `timeline().is_flushing()` or
    /// `timeline().is_seek_pending()` is true. Reads target/epoch from
    /// Timeline, performs the seek on the decoder, then calls
    /// `timeline().clear_seek_pending(epoch)` on success.
    ///
    /// Returns `true` if the seek was applied (or abandoned after max retries),
    /// `false` if it will be retried on the next iteration.
    fn apply_pending_seek(&mut self) -> bool;

    /// Check whether the source has enough data for a non-blocking `fetch_next()`.
    ///
    /// Used by the shared worker scheduler to decide whether to call
    /// `fetch_next()` on this track. Returns `true` when the underlying
    /// stream has data ready for the current read position.
    fn is_ready(&self) -> bool;
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
