use kithara_signal::{AudioChunk, AudioSpec};

use crate::{
    EffectAutomation, EffectChange, EffectParameter, EffectPrepareError, EffectRefusal,
    EffectTarget,
};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Audio processing effect in the chain (transforms decoded-audio chunks).
#[kithara::mock(api = AudioEffectMock)]
pub trait AudioEffect: Send + 'static {
    /// Whether every input frame produces exactly one output frame, with no flush PCM.
    fn preserves_frames(&self) -> bool {
        false
    }

    /// Whether the installed state admits explicit values for this field.
    fn admits_automation(
        &self,
        parameter: EffectParameter,
        values: std::ops::RangeInclusive<f64>,
    ) -> bool {
        let _ = (parameter, values);
        false
    }

    /// Process one frame-preserving block without target smoothing.
    /// # Errors
    /// Unsupported stages refuse; admission must precede any processing.
    fn process_automated(
        &mut self,
        chunk: AudioChunk,
        controls: &[EffectAutomation],
        effect_index: usize,
    ) -> Result<AudioChunk, EffectRefusal> {
        let _ = (chunk, controls, effect_index);
        Err(EffectRefusal)
    }

    /// Apply one prepared field without clearing history or allocating.
    /// # Errors
    /// Refuses unsupported targets or contextual bounds without changing state.
    fn configure(
        &mut self,
        target: EffectTarget,
        change: EffectChange,
    ) -> Result<(), EffectRefusal> {
        let _ = (target, change);
        Err(EffectRefusal)
    }

    /// Flush one remaining buffered chunk at end of stream.
    ///
    /// Repeated calls must reach `None` after a finite number of chunks and
    /// remain exhausted until [`process`](Self::process) or
    /// [`reset`](Self::reset) starts a new effect lifecycle. Each call is a
    /// bounded real-time transition: it must not block, allocate, free, or
    /// rebuild backend state.
    fn flush(&mut self) -> Option<AudioChunk>;

    /// Whether the prepared state admits this format after deferred service.
    /// Fixed-format effects refuse incompatible streams before production.
    fn accepts_spec(&self, spec: AudioSpec) -> bool {
        let _ = spec;
        true
    }

    /// Frames accepted by this effect but not yet represented in its output,
    /// counted on the decoded-source axis.
    ///
    /// There is no default: an effect that buffers must expose its hold so a
    /// decoder recreation cannot resume past audio that has not been emitted.
    /// Behind a duration-changing Warp stage this value must remain on the
    /// decoded-source axis; an effect that cannot report that hold must not
    /// buffer there.
    fn held_source_frames(&self) -> u64;

    /// Process an audio chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output
    /// yet). This is a bounded real-time transition and must not block,
    /// allocate, free, or rebuild backend state.
    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk>;

    /// Reset internal state after a seek using only bounded real-time work.
    /// Allocation, destruction, and backend rebuilding belong in
    /// [`service_deferred`](Self::service_deferred).
    fn reset(&mut self);

    /// Service work deferred by the checked audio-production path.
    ///
    /// The dispatcher shell calls this after lifecycle transitions, outside
    /// the checked decoder tick, using the active
    /// decoder specification. Effects without deferred work can keep the
    /// default no-op implementation.
    /// # Errors
    /// Refuses a format or failed preparation before further PCM consumption.
    fn service_deferred(&mut self, spec: AudioSpec) -> Result<(), EffectPrepareError> {
        let _ = spec;
        Ok(())
    }
}
