use kithara::{
    analysis::{AnalysisDemand, AnalysisFingerprint, AnalysisProgress, BeatGridModel, MusicalKey},
    platform::sync::Arc,
    play::{ArtifactLoadError, ArtifactSource},
    waveform::Waveform,
};

use crate::pools::AppResourceConfig;

/// What a track already holds for one artifact, before any analysis runs.
///
/// Only [`Self::Missing`] is analysis's to produce. A track opened with an
/// explicit source asked for that artifact from there: while it loads the
/// artifact is [`Self::Pending`], and a load that fails leaves it
/// [`Self::Failed`] — neither silently becomes local work.
#[derive(Clone, Debug, Default)]
pub(crate) enum Supply<T> {
    /// A value the caller handed over, ready to publish.
    Ready(Arc<T>),
    /// An external source is being read; the artifact is neither here yet nor
    /// anyone else's to produce.
    Pending,
    /// The external source did not yield a usable artifact. The track stays
    /// without it and says so.
    Failed,
    /// Nothing prepared: this artifact is analysis's to produce.
    #[default]
    Missing,
}

impl<T> Supply<T> {
    /// Whether analysis still owes this artifact.
    pub(crate) const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// Whether an external source is still being read for this artifact.
    pub(crate) const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub(crate) const fn value(&self) -> Option<&Arc<T>> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Pending | Self::Failed | Self::Missing => None,
        }
    }
}

impl<T> From<Option<&ArtifactSource<T>>> for Supply<T> {
    fn from(source: Option<&ArtifactSource<T>>) -> Self {
        match source {
            Some(ArtifactSource::Value(value)) => Self::Ready(Arc::clone(value)),
            Some(ArtifactSource::Source(_)) => Self::Pending,
            _ => Self::Missing,
        }
    }
}

impl<T> From<Result<Arc<T>, ArtifactLoadError>> for Supply<T> {
    fn from(loaded: Result<Arc<T>, ArtifactLoadError>) -> Self {
        loaded.map_or(Self::Failed, Self::Ready)
    }
}

/// Every prepared artifact one track was opened with.
#[derive(Clone, Debug, Default)]
pub(crate) struct Prepared {
    pub(crate) beat_grid: Supply<BeatGridModel>,
    pub(crate) key: Supply<MusicalKey>,
    pub(crate) waveform: Supply<Waveform>,
}

impl Prepared {
    /// The artifacts a pass is opened for: what this runtime can analyse at
    /// all, minus what the track already carries. A checkpoint does not narrow
    /// this — a resumed pass continues the very artifacts it was opened for.
    pub(crate) fn demand(&self, fingerprint: &AnalysisFingerprint) -> AnalysisDemand {
        let mut demand = AnalysisDemand::empty();
        demand.set(
            AnalysisDemand::BEAT,
            fingerprint.beat().is_some() && self.beat_grid.is_missing(),
        );
        demand.set(
            AnalysisDemand::WAVEFORM,
            fingerprint.waveform().is_some() && self.waveform.is_missing(),
        );
        demand.set(
            AnalysisDemand::KEY,
            fingerprint.key().is_some() && self.key.is_missing(),
        );
        demand
    }

    pub(crate) fn for_config(config: &AppResourceConfig) -> Self {
        Self {
            beat_grid: config.beat_grid().into(),
            key: config.key().into(),
            waveform: config.waveform().into(),
        }
    }

    /// Whether the track was opened with nothing prepared.
    pub(crate) const fn is_empty(&self) -> bool {
        self.beat_grid.is_missing() && self.key.is_missing() && self.waveform.is_missing()
    }

    /// Whether the track needs nothing further: the pass ran its course and
    /// every artifact this runtime offers is either prepared or published.
    /// A prepared artifact excuses the pass from producing it. An analysed key
    /// is published once the snapshot carries its tag, with or without a key.
    pub(crate) fn settled_for(
        &self,
        progress: &AnalysisProgress,
        fingerprint: &AnalysisFingerprint,
    ) -> bool {
        let analysis = progress.analysis();
        let beat = !self.beat_grid.is_missing()
            || fingerprint.beat().is_none()
            || analysis.beat().is_some();
        let waveform = !self.waveform.is_missing()
            || fingerprint.waveform().is_none()
            || analysis.waveform().is_some();
        let key = !self.key.is_missing()
            || fingerprint.key().is_none()
            || analysis.fingerprint().key().is_some();
        analysis.is_settled() && beat && waveform && key
    }
}
