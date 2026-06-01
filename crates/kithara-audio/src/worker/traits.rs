use kithara_decode::PcmChunk;
use kithara_stream::Timeline;

use crate::{pipeline::track_fsm, traits::AudioEffect};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Trait for audio sources processed in a blocking worker thread.
///
/// The worker calls `step_track()` once per scheduling round. Each call
/// performs at most one FSM transition and returns a [`TrackStep`] that
/// tells the worker what happened.
#[kithara::mock(api = MockAudioWorkerSource, type Chunk = PcmChunk;)]
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

    /// Drain deferred off-core signals armed on the forbid-blocking decode
    /// core: reader-hook events (published to the event bus) and the reader→peer
    /// wake (a cross-thread `notify_one` the RT core must not make). The worker
    /// shell calls this once per pass, off the checked path. Default no-op.
    fn flush_deferred(&mut self) {}

    /// One-time worker-thread warmup, called from the scheduler shell when the
    /// node registers. Pre-touches the produce-core read path so lazy global
    /// thread-locals (the `arc_swap` committed-read debt node) allocate here,
    /// off the forbid-blocking decode core. Default no-op.
    fn warm_up(&mut self) {}
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

/// Fully drain the effect chain at the end of the stream.
///
/// A buffering effect can hold a tail that needs several `flush()` pulls, and that tail must
/// still pass through every downstream effect.
pub(crate) fn drain_effects(effects: &mut [Box<dyn AudioEffect>]) -> Vec<PcmChunk> {
    let mut carry: Vec<PcmChunk> = Vec::new();
    for effect in &mut *effects {
        let mut produced: Vec<PcmChunk> = Vec::new();
        for chunk in carry.drain(..) {
            if let Some(out) = effect.process(chunk) {
                produced.push(out);
            }
        }
        while let Some(out) = effect.flush() {
            produced.push(out);
        }
        carry = produced;
    }
    carry
}

/// Reset effects chain (e.g. after seek).
pub(crate) fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in &mut *effects {
        effect.reset();
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::traits::AudioEffectMock;

    const SPEC: PcmSpec = PcmSpec {
        channels: 1,
        sample_rate: 48_000,
    };

    fn chunk(range: std::ops::Range<u32>) -> PcmChunk {
        let samples: Vec<f32> = range.map(|i| i as f32).collect();
        PcmChunk::new(
            PcmMeta {
                spec: SPEC,
                frames: u32::try_from(samples.len()).unwrap(),
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    #[kithara::test]
    fn drain_effects_extracts_full_multi_stage_tail() {
        // Stage 0's tail comes out over 3 `flush()` pulls; each must be
        // fed through stage 1's `process()`, then stage 1 must itself
        // be flushed to exhaustion. A single-pass flush would stop after
        // the first stage-0 chunk and truncate the rest.
        let stage0 = Box::new(Unimock::new((
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(0..200))),
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(200..400))),
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(400..600))),
            AudioEffectMock::flush.next_call(matching!()).returns(None),
        )));
        let stage1 = Box::new(Unimock::new((
            AudioEffectMock::process
                .each_call(matching!())
                .returns(None), // buffers all 3 carried chunks
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(0..250))),
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(250..500))),
            AudioEffectMock::flush
                .next_call(matching!())
                .returns(Some(chunk(500..600))),
            AudioEffectMock::flush.next_call(matching!()).returns(None),
        )));

        let mut chain: Vec<Box<dyn AudioEffect>> = vec![stage0, stage1];
        let out = drain_effects(&mut chain);
        let total: Vec<f32> = out.iter().flat_map(|c| c.pcm.iter().copied()).collect();

        assert!(
            out.len() > 1,
            "tail spans multiple flush pulls (got {})",
            out.len()
        );
        let expected: Vec<f32> = (0..600).map(|i| i as f32).collect();
        assert_eq!(total, expected, "no tail truncation across the chain");
    }

    #[kithara::test]
    fn drain_effects_empty_chain_for_non_buffering() {
        let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();
        assert!(drain_effects(&mut chain).is_empty());
    }
}
