use kithara_bufpool::HasPool;
use kithara_platform::ranged;

use super::{ElasticCapabilities, ElasticConfig, ElasticDrain, ElasticError, ElasticRequest};

ranged!(
    /// Valid native pitch factor shared by every elastic backend.
    pub(crate) struct PitchScale(f64, 0.25, 4.0, 1.0)
);

/// Exact-span time-stretch engine.
///
/// The caller owns the transport: every call names the source span and the
/// output span in frames, and the frame counts are the only rate control. An
/// engine never chooses a rate, a direction or a phase on its own, so two
/// engines fed the same plan advance through the source identically.
pub trait ElasticEngine: Send + 'static {
    /// Immutable limits, latency and rate window of this engine.
    fn capabilities(&self) -> ElasticCapabilities;

    /// Writes the next portion of terminal buffered audio into caller-owned storage.
    ///
    /// The caller chooses the non-empty whole-frame storage capacity and repeats
    /// until the returned [`ElasticDrain`] is complete. Every incomplete step
    /// writes a non-empty contiguous tail portion no larger than that capacity;
    /// an active drain reports completion together with its final non-empty
    /// portion. Fresh, reset and already-completed engines return an empty
    /// completed step until [`prime`](Self::prime) or [`process`](Self::process).
    ///
    /// # Errors
    /// While a drain is active, returns [`ElasticError`] when `output` is empty,
    /// does not contain a whole number of interleaved frames, or when sizing a
    /// span overflows. An inactive drain returns an empty completed step without
    /// accessing `output`.
    fn flush(&mut self, output: &mut [f32]) -> Result<ElasticDrain, ElasticError>;

    /// Allocates and initializes an engine for a fixed preparation shape,
    /// outside the render core.
    ///
    /// # Errors
    /// Returns [`ElasticError`] when the shape is outside what the engine can
    /// represent, or when the engine cannot be constructed.
    fn prepare<S>(config: ElasticConfig<S>) -> Result<Self, ElasticError>
    where
        Self: Sized,
        S: HasPool<f32>;

    /// Clears prior stream state, absorbs source history and lookahead, then
    /// renders one latency-sized warmup span into caller-owned discard storage.
    ///
    /// `source_lookahead` contains exactly the declared source latency starting
    /// at the audible cue; `source` follows it at the rate named by `request`.
    /// Source passed to the next [`process`](Self::process) follows `source`,
    /// while its first output resumes at the start of `source_lookahead` after
    /// the engine latency has been absorbed.
    ///
    /// # Errors
    /// Returns [`ElasticError`] when the warmup request does not match the
    /// declared latency, when a buffer length does not match the request, or
    /// when the rate is outside the declared envelope.
    fn prime(
        &mut self,
        request: ElasticRequest,
        source_history: &[f32],
        source_lookahead: &[f32],
        source: &[f32],
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError>;

    /// Renders exactly `request.output_frames()` interleaved output frames
    /// from exactly `request.source_frames()` interleaved source frames.
    /// Across this and any immediately adjacent calls, a changed ratio must
    /// affect emitted audio within `capabilities().latency().output_frames()`
    /// frames. Engines must not add software-buffering delay beyond their
    /// declared native latency.
    ///
    /// # Errors
    /// Returns [`ElasticError`] when the request is outside the prepared
    /// limits or the declared rate envelope, when a buffer length does not
    /// match the request, or when the engine renders a different span.
    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError>;

    /// Clears stream history while retaining the prepared shape and latency.
    ///
    /// # Errors
    /// Returns [`ElasticError`] when the resident backend state cannot be cleared.
    fn reset(&mut self) -> Result<(), ElasticError>;

    /// Sets pitch independently from source-to-output frame advance. Across
    /// immediately adjacent [`process`](Self::process) calls, a changed pitch
    /// must affect emitted audio within the declared output latency; engines
    /// must not add a second software-buffering delay.
    ///
    /// # Errors
    /// Returns [`ElasticError`] when `scale` is outside the common native
    /// range `0.25..=4.0` or is not finite.
    fn set_pitch(&mut self, scale: f64) -> Result<(), ElasticError>;
}
