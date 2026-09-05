use std::num::NonZeroU32;

use bon::Builder;
use kithara_platform::sync::Arc;

use super::snapshot::BeatSnapshot;
use crate::{
    coverage::{Coverage, FrameRange},
    waveform::bucket::Waveform,
};

/// Opaque identity the caller opens a pass with, echoed on every snapshot and
/// never interpreted here: track identity belongs to the caller.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnalysisToken(Arc<str>);

impl AnalysisToken {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AnalysisToken {
    fn from(token: &str) -> Self {
        Self(Arc::from(token))
    }
}

impl From<String> for AnalysisToken {
    fn from(token: String) -> Self {
        Self(Arc::from(token))
    }
}

/// What produced a snapshot, per artifact. The two are separate so a change to
/// one analyzer's configuration cannot invalidate the other's stored results.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct AnalysisFingerprint {
    beat: Option<Arc<str>>,
    waveform: Option<Arc<str>>,
}

impl AnalysisFingerprint {
    #[must_use]
    pub fn new(beat: Option<&str>, waveform: Option<&str>) -> Self {
        Self {
            beat: beat.map(Arc::from),
            waveform: waveform.map(Arc::from),
        }
    }

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
}

/// One publication of an analysis pass: self-contained, so a consumer holding
/// only this can render the waveform, place markers on the source timeline, and
/// tell how much of the track it is based on.
#[derive(Builder, Clone, Debug)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct TrackAnalysis {
    #[builder(default)]
    fingerprint: AnalysisFingerprint,
    token: AnalysisToken,
    #[builder(default)]
    coverage: Coverage,
    source_sample_rate: NonZeroU32,
    beat: Option<BeatSnapshot>,
    extent: Option<u64>,
    waveform: Option<Waveform>,
    #[builder(default)]
    settled: bool,
    revision: u64,
}

impl TrackAnalysis {
    #[must_use]
    pub const fn beat(&self) -> Option<&BeatSnapshot> {
        self.beat.as_ref()
    }

    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Source length in frames, when the pass knows it.
    #[must_use]
    pub const fn extent(&self) -> Option<u64> {
        self.extent
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &AnalysisFingerprint {
        &self.fingerprint
    }

    /// Whether the whole known extent sits in one covered run.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.extent
            .is_some_and(|extent| self.coverage.contains(FrameRange::new(0, extent)))
    }

    /// Whether the pass ran out of positions the source can deliver. A gap
    /// that stays is one no read could fill: a seek that lands elsewhere, a
    /// head the decoder cannot deliver. A pass its reader cut short is not
    /// settled.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.settled
    }

    /// Source ranges no producer covered, derived from the coverage rather
    /// than recorded. The horizon is the extent when known and the covered
    /// frontier until then, as [`source_frames`](Self::source_frames) uses.
    #[must_use]
    pub fn missing(&self) -> Vec<FrameRange> {
        self.coverage
            .gaps(self.extent.unwrap_or_else(|| self.coverage.frontier()))
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The denominator that turns an artifact frame into a fraction: the extent
    /// when known, and the covered frontier otherwise - the same "what is
    /// known to exist" rule [`missing`](Self::missing) uses. Counting covered
    /// frames instead would put a marker past the end of a coverage set that
    /// is spread over the source rather than grown from its start.
    #[must_use]
    pub fn source_frames(&self) -> u64 {
        self.extent.unwrap_or_else(|| self.coverage.frontier())
    }

    #[must_use]
    pub const fn source_sample_rate(&self) -> NonZeroU32 {
        self.source_sample_rate
    }

    #[must_use]
    pub const fn token(&self) -> &AnalysisToken {
        &self.token
    }

    #[must_use]
    pub const fn waveform(&self) -> Option<&Waveform> {
        self.waveform.as_ref()
    }
}
