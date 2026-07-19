use super::{ElasticCapabilities, ElasticError, ElasticRequest};

mod sealed {
    pub trait Sealed {}
}

#[cfg(feature = "stretch-signalsmith")]
impl sealed::Sealed for super::SignalsmithElastic {}

/// Prepared, sealed time-stretch engine with caller-owned audio storage.
/// Consumers may store trait objects; construction and validation remain crate-owned.
pub trait ElasticBackend: sealed::Sealed + Send + 'static {
    /// Returns the immutable limits and algorithmic latency of this engine.
    fn capabilities(&self) -> ElasticCapabilities;

    /// Resets state and seeds exact history and warmup spans into `discarded_output`.
    /// # Errors
    /// Returns [`ElasticError`] for invalid latency, shapes, arithmetic, or rates.
    fn prime(
        &mut self,
        request: ElasticRequest,
        source_history: &[f32],
        source: &[f32],
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError>;

    /// Processes exact source and output spans; frame counts are the sole rate control.
    /// # Errors
    /// Returns [`ElasticError`] for invalid shapes, limits, arithmetic, or rates.
    fn process(
        &mut self,
        request: ElasticRequest,
        source: &[f32],
        output: &mut [f32],
    ) -> Result<(), ElasticError>;

    /// Clears stream history while retaining the prepared shape and latency.
    /// Implementations must reuse prepared storage and perform work bounded by that
    /// shape, without allocation, blocking, I/O, or logging.
    fn reset(&mut self);
}
