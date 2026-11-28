use bon::Builder;
use kithara_platform::sync::Arc;

/// What produced a snapshot, per artifact. The tags are separate so a change
/// to one analyzer's configuration cannot invalidate another's stored results.
#[derive(Builder, Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct AnalysisFingerprint {
    #[builder(into)]
    beat: Option<Arc<str>>,
    #[builder(into)]
    waveform: Option<Arc<str>>,
    #[builder(into)]
    key: Option<Arc<str>>,
}

impl AnalysisFingerprint {
    /// Beat backend, model, and artifact semantics.
    #[must_use]
    pub fn beat(&self) -> Option<&str> {
        self.beat.as_deref()
    }

    /// Waveform analyzer configuration.
    #[must_use]
    pub fn waveform(&self) -> Option<&str> {
        self.waveform.as_deref()
    }

    /// Key estimator configuration and state version.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}
