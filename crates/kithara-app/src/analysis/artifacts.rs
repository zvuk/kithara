use kithara::{
    analysis::{AnalysisToken, BeatGridModel, MusicalKey, TrackAnalysis, Waveform},
    link::GridRefusal,
};

/// Identity of an artifact a publication carries: the pass revision that
/// produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactId {
    Analysed(AnalysisToken, u64),
}

/// A publication's word on its track's grid: one may still come, here it is,
/// or none will.
pub(crate) enum GridState<'a> {
    Pending,
    Ready(&'a BeatGridModel),
    Refused(GridRefusal),
}

/// One track's published artifacts: its local analysis result, and whether
/// the pass that was to produce it failed.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackArtifacts {
    analysis: Option<TrackAnalysis>,
    pass_failed: bool,
}

impl TrackArtifacts {
    pub(crate) const fn new(analysis: Option<TrackAnalysis>, pass_failed: bool) -> Self {
        Self {
            analysis,
            pass_failed,
        }
    }

    delegate::delegate! {
        to self.analysis {
            /// The local analysis result, when a pass produced one.
            #[call(as_ref)]
            pub(crate) const fn analysis(&self) -> Option<&TrackAnalysis>;
            /// The beat grid to paint and to clock against.
            #[expr($.and_then(TrackAnalysis::grid))]
            #[call(as_ref)]
            pub(crate) fn grid(&self) -> Option<&BeatGridModel>;
            /// The track's analysed key.
            #[expr(key_of($))]
            #[call(as_ref)]
            pub(crate) fn key(&self) -> Option<MusicalKey>;
            /// The waveform to draw.
            #[expr($.and_then(TrackAnalysis::waveform))]
            #[call(as_ref)]
            pub(crate) fn waveform(&self) -> Option<&Waveform>;
        }
    }

    /// What this publication settles on for its track's grid.
    pub(crate) fn grid_state(&self) -> GridState<'_> {
        let refused = |reason: &str| {
            GridState::Refused(GridRefusal {
                reason: reason.to_owned(),
            })
        };
        match self.grid() {
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
        }
    }

    /// What names the waveform this publication draws.
    pub(crate) fn waveform_id(&self) -> Option<ArtifactId> {
        self.analysis
            .as_ref()
            .filter(|analysis| analysis.waveform().is_some())
            .map(|analysis| ArtifactId::Analysed(analysis.token().clone(), analysis.revision()))
    }
}

/// The key an analysis heard, when it heard one.
pub(super) fn key_of(analysis: Option<&TrackAnalysis>) -> Option<MusicalKey> {
    analysis
        .and_then(TrackAnalysis::key)
        .map(|estimate| estimate.key())
}

impl From<TrackAnalysis> for TrackArtifacts {
    fn from(analysis: TrackAnalysis) -> Self {
        Self::new(Some(analysis), false)
    }
}
