use kithara::{
    analysis::{AnalysisToken, BeatGridModel, TrackAnalysis, Waveform},
    link::GridRefusal,
    platform::sync::Arc,
};

use super::supply::{Prepared, Supply};

/// Identity of an artifact a publication carries. A supplied artifact is
/// identified by the value the caller handed over, which never changes under
/// the track; an analysed one by the pass revision that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactId {
    Supplied(usize),
    Analysed(AnalysisToken, u64),
}

/// A publication's word on its track's grid: one may still come, here it is,
/// or none will.
pub(crate) enum GridState<'a> {
    Pending,
    Ready(&'a BeatGridModel),
    Refused(GridRefusal),
}

/// One track's published artifacts, whatever their origin: a beat grid or a
/// waveform the caller supplied, a local analysis result, or one of each.
///
/// A consumer asks for an artifact, not for where it came from. A prepared
/// value outranks an analysed one for the same artifact: analysis is only ever
/// opened for what no prepared value covers, so the two cannot disagree.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackArtifacts {
    analysis: Option<TrackAnalysis>,
    prepared: Prepared,
    pass_failed: bool,
}

impl TrackArtifacts {
    pub(crate) fn new(
        analysis: Option<TrackAnalysis>,
        prepared: Prepared,
        pass_failed: bool,
    ) -> Self {
        Self {
            analysis,
            prepared,
            pass_failed,
        }
    }

    delegate::delegate! {
        to self.analysis {
            /// The local analysis result, when a pass produced one. Facts that belong
            /// to the pass itself — coverage, extent, what it settled on — are read
            /// from here; artifacts are read from this type instead, which does not
            /// care which origin served them.
            #[call(as_ref)]
            pub(crate) const fn analysis(&self) -> Option<&TrackAnalysis>;
            /// The beat grid to paint and to clock against.
            #[expr(Self::grid_from($, &self.prepared))]
            #[call(as_ref)]
            pub(crate) fn grid(&self) -> Option<&BeatGridModel>;
        }
    }

    pub(super) fn grid_from<'a>(
        analysis: Option<&'a TrackAnalysis>,
        prepared: &'a Prepared,
    ) -> Option<&'a BeatGridModel> {
        prepared
            .beat_grid
            .value()
            .map(Arc::as_ref)
            .or_else(|| analysis.and_then(TrackAnalysis::grid))
    }

    /// What this publication settles on for its track's grid.
    pub(crate) fn grid_state(&self) -> GridState<'_> {
        let refused = |reason: &str| {
            GridState::Refused(GridRefusal {
                reason: reason.to_owned(),
            })
        };
        match &self.prepared.beat_grid {
            Supply::Ready(grid) => GridState::Ready(grid.as_ref()),
            Supply::Pending => GridState::Pending,
            Supply::Failed => refused("the supplied beat grid did not load"),
            Supply::Missing => match self.analysis.as_ref().and_then(TrackAnalysis::grid) {
                Some(grid) => GridState::Ready(grid),
                None if self.pass_failed => refused("the analysis pass failed"),
                None if self
                    .analysis
                    .as_ref()
                    .is_some_and(TrackAnalysis::is_settled) =>
                {
                    refused("the analysis found no beat grid")
                }
                None => GridState::Pending,
            },
        }
    }

    /// The waveform to draw.
    pub(crate) fn waveform(&self) -> Option<&Waveform> {
        self.prepared
            .waveform
            .value()
            .map(Arc::as_ref)
            .or_else(|| self.analysis.as_ref().and_then(TrackAnalysis::waveform))
    }

    /// What names the waveform this publication draws.
    pub(crate) fn waveform_id(&self) -> Option<ArtifactId> {
        self.prepared.waveform.value().map(supplied).or_else(|| {
            self.analysis
                .as_ref()
                .filter(|analysis| analysis.waveform().is_some())
                .map(analysed)
        })
    }
}

fn supplied<T>(value: &Arc<T>) -> ArtifactId {
    ArtifactId::Supplied(Arc::as_ptr(value).cast::<()>() as usize)
}

fn analysed(analysis: &TrackAnalysis) -> ArtifactId {
    ArtifactId::Analysed(analysis.token().clone(), analysis.revision())
}

impl From<TrackAnalysis> for TrackArtifacts {
    fn from(analysis: TrackAnalysis) -> Self {
        Self::new(Some(analysis), Prepared::default(), false)
    }
}
