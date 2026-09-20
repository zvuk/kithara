use std::num::NonZeroU32;

use bon::Builder;
use kithara_platform::sync::Arc;
use kithara_warp::{AssetAxis, MapPosition, SegmentSet};
use num_traits::ToPrimitive;

use super::snapshot::BeatSnapshot;
use crate::{
    coverage::{Coverage, FrameRange},
    segments::{BeatGridError, GridBeat, segment_set},
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

    /// The warp grid this analysis states for the track, on the source axis.
    ///
    /// `None` until a beat pass has run over a track whose extent is known:
    /// the grid is laid on the declared source axis, and a covered frontier is
    /// not that axis. The grid answers over the whole axis, extending the
    /// spans the pass left unmarked from the beats it did mark.
    ///
    /// # Errors
    ///
    /// Returns [`BeatGridError`] when the beats the pass found cannot form a
    /// grid on the source axis.
    #[must_use]
    pub fn beat_grid(&self) -> Option<Result<SegmentSet, BeatGridError>> {
        let beat = self.beat.as_ref()?;
        let frames = self.extent?;
        Some(segment_set(
            beat.artifact(),
            AssetAxis::new(self.source_sample_rate, frames),
        ))
    }

    /// Every beat the grid states, as its ordinal and the source frame it
    /// sounds at, in order.
    ///
    /// `None` on the same terms as [`beat_grid`](Self::beat_grid). This is the
    /// sequence a caller draws or counts; the grid itself answers geometry.
    ///
    /// # Errors
    ///
    /// Returns [`BeatGridError`] when the beats the pass found cannot form a
    /// grid on the source axis, and [`BeatGridError::OffSourceAxis`] when the
    /// grid answers a position that axis cannot express.
    #[must_use]
    pub fn beats(&self) -> Option<Result<Vec<GridBeat>, BeatGridError>> {
        let grid = match self.beat_grid()? {
            Ok(grid) => grid,
            Err(error) => return Some(Err(error)),
        };
        Some(
            grid.beats()
                .map(|(beat, position)| {
                    let ordinal = f64::from(beat).round().to_i64();
                    let frame = match position {
                        MapPosition::Asset(frame) => f64::from(frame).round().to_u64(),
                        _ => None,
                    };
                    ordinal
                        .zip(frame)
                        .map(|(ordinal, frame)| GridBeat::new(ordinal, frame))
                        .ok_or(BeatGridError::OffSourceAxis)
                })
                .collect(),
        )
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{AnalysisToken, BeatSnapshot, NonZeroU32, TrackAnalysis};
    use crate::{BeatArtifact, BeatState, GridBeat};

    struct Consts;

    impl Consts {
        const EXTENT: u64 = 48_000;
        const SAMPLE_RATE: u32 = 48_000;
    }

    fn analysis(extent: Option<u64>) -> TrackAnalysis {
        let beats = (0..4).map(|beat| (beat * 12_000, Some(1.0))).collect();
        let artifact = BeatArtifact::new(120.0, beats, Vec::new());
        TrackAnalysis::builder()
            .token(AnalysisToken::from("fixture"))
            .source_sample_rate(
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("invariant: fixture rate is non-zero"),
            )
            .beat(BeatSnapshot::new(artifact, BeatState::Final, Vec::new()))
            .maybe_extent(extent)
            .revision(0)
            .build()
    }

    /// Every beat carries the ordinal its grid gives it.
    ///
    /// The beats a pass leaves unmarked are extended from the beats it marked,
    /// and the head reaches back from the first marked beat, so the ordinals
    /// before it are negative. A caller counts and draws by these numbers, so
    /// they must be the grid's own rather than a position in a list.
    #[kithara::test]
    fn a_beat_carries_the_ordinal_of_its_grid() {
        let beats = analysis(Some(Consts::EXTENT))
            .beats()
            .expect("the stated extent carries a grid")
            .expect("the fixture beats form a grid");

        assert_eq!(
            beats,
            [(0, 0), (1, 12_000), (2, 24_000), (3, 36_000), (4, 48_000)]
                .map(|(ordinal, frame)| GridBeat::new(ordinal, frame))
                .to_vec(),
            "the grid states one beat per marked interval and extends its tail"
        );
    }

    /// A grid is laid on the extent the track declares, never on a frontier.
    ///
    /// A covered frontier moves while a pass runs, so a grid built on it would
    /// answer over an axis the track does not have, and the beats past that
    /// frontier would be refused as outside the extent.
    #[kithara::test]
    fn a_grid_exists_only_once_the_track_states_its_extent() {
        assert!(
            analysis(None).beat_grid().is_none(),
            "no extent is stated, so the analysis states no grid"
        );
        assert!(
            analysis(Some(Consts::EXTENT))
                .beat_grid()
                .expect("the stated extent carries a grid")
                .is_ok(),
            "the stated extent carries the grid the beats form"
        );
    }
}
