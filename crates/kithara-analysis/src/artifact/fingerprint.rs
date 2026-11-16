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

    /// Whether a result made under these tags serves where `active` is in
    /// force: a pass opened for part of what `active` produces. At least one
    /// tag is present, and each present tag equals the active one.
    pub(crate) fn serves(&self, active: &Self) -> bool {
        let tags = [
            (self.beat(), active.beat()),
            (self.waveform(), active.waveform()),
            (self.key(), active.key()),
        ];
        tags.iter().any(|(own, _)| own.is_some())
            && tags
                .iter()
                .all(|(own, active)| own.is_none() || own == active)
    }
}
