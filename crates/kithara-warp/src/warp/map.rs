use super::WarpCursor;
use crate::{
    AssetFrame, BeatAlignment, BeatGridQuery, BeatGridSnapshot, BeatGridUnavailable,
    GridProjectionError, MapPoint, MapPosition, SessionFrame, WarpMapRevision,
};

/// One immutable session-output-to-source map revision.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct WarpMap {
    projection: Option<ProjectedMap>,
    /// Immutable owner-assigned map revision.
    #[field(get, copy)]
    revision: WarpMapRevision,
}

#[derive(Clone, Debug, PartialEq)]
struct ProjectedMap {
    grid: BeatGridSnapshot,
    source: BeatGridSnapshot,
}

impl WarpMap {
    /// Creates an immutable identity-map revision.
    #[must_use]
    pub const fn identity(revision: WarpMapRevision) -> Self {
        Self {
            revision,
            projection: None,
        }
    }

    /// Freezes a recording projected onto a session grid using stamped alignment.
    ///
    /// # Errors
    /// Returns the M3 projection error without replacing missing geometry.
    pub fn projected(
        source: BeatGridSnapshot,
        target: BeatGridSnapshot,
        alignment: BeatAlignment,
        revision: WarpMapRevision,
    ) -> Result<Self, GridProjectionError> {
        let grid = BeatGridSnapshot::projection(source.clone(), target, alignment)?;
        Ok(Self {
            revision,
            projection: Some(ProjectedMap { grid, source }),
        })
    }

    /// Source frames per session output frame, including the sample-rate relation.
    pub fn rate_at(&self, output: SessionFrame) -> BeatGridQuery<f64> {
        let Some(projection) = &self.projection else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry);
        };
        projection.grid.rate_at(MapPoint::new(
            projection.grid.stamp(),
            MapPosition::Session(output),
        ))
    }

    /// Creates renderer-local progress at an exact discontinuity boundary.
    #[must_use]
    pub const fn reanchor(&self, source: u64, output: SessionFrame) -> WarpCursor {
        WarpCursor::new(self.revision, source, output)
    }

    /// Absolute recording frame sounding at an output frame.
    pub fn source_at(&self, output: SessionFrame) -> BeatGridQuery<AssetFrame> {
        let Some(projection) = &self.projection else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry);
        };
        projection.grid.source_at(MapPoint::new(
            projection.grid.stamp(),
            MapPosition::Session(output),
        ))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn reanchor_carries_the_map_revision_and_exact_frontier() {
        let revision = WarpMapRevision::first();
        let map = WarpMap::identity(revision);
        let cursor = map.reanchor(80, SessionFrame::new(120));

        assert_eq!(map.revision(), revision);
        assert_eq!(cursor.revision(), revision);
        assert_eq!(cursor.source(), 80);
        assert_eq!(cursor.output(), SessionFrame::new(120));
    }
    #[cfg(feature = "render")]
    #[kithara::test]
    fn projected_map_uses_absolute_source_endpoints() {
        use std::num::NonZeroU32;

        use crate::{Beat, BeatGridQuery, mock};
        let rate = NonZeroU32::new(48_000).expect("rate");
        let source = mock::asset_grid(120.0, rate);
        let target = mock::session_grid(180.0, rate);
        let cue = Beat::new(0.0).expect("finite cue");
        let alignment = BeatAlignment::new(
            MapPoint::new(source.stamp(), cue),
            MapPoint::new(target.stamp(), cue),
        );
        let plan = WarpMap::projected(source, target, alignment, WarpMapRevision::first())
            .expect("compatible axes");
        let BeatGridQuery::Resolved(source) = plan.source_at(SessionFrame::new(128)) else {
            panic!("projected source resolves");
        };
        assert!((f64::from(source) - 192.0).abs() < 1e-10);
        assert_eq!(
            plan.rate_at(SessionFrame::new(128)),
            BeatGridQuery::Resolved(1.5)
        );
    }

    #[cfg(feature = "render")]
    #[kithara::test]
    fn projected_rate_includes_the_source_to_output_sample_rate_relation() {
        use std::num::NonZeroU32;

        use crate::{Beat, mock};

        let source = mock::asset_grid(120.0, NonZeroU32::new(44_100).expect("source rate"));
        let target = mock::session_grid(180.0, NonZeroU32::new(48_000).expect("output rate"));
        let cue = Beat::new(0.0).expect("finite cue");
        let alignment = BeatAlignment::new(
            MapPoint::new(source.stamp(), cue),
            MapPoint::new(target.stamp(), cue),
        );
        let map = WarpMap::projected(source, target, alignment, WarpMapRevision::first())
            .expect("compatible axes");
        let BeatGridQuery::Resolved(source) = map.source_at(SessionFrame::new(48_000)) else {
            panic!("source endpoint resolves");
        };
        let BeatGridQuery::Resolved(rate) = map.rate_at(SessionFrame::new(48_000)) else {
            panic!("derivative resolves");
        };
        assert!((f64::from(source) - 66_150.0).abs() < 1e-8);
        assert!((rate - 1.5 * 44_100.0 / 48_000.0).abs() < 1e-12);
    }
}
