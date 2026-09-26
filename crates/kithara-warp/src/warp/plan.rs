use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;

use crate::{AssetFrame, BeatGridQuery, MapAxis, SessionFrame, WarpCursor, WarpMap};

/// A resident projected map cannot resolve its activation.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum WarpPlanError {
    /// The recording geometry refused the activation coordinate.
    #[error("projected activation has no source position: {0:?}")]
    Source(BeatGridQuery<AssetFrame>),
    /// The activation rate is unavailable.
    #[error("projected activation has no rate: {0:?}")]
    Rate(BeatGridQuery<f64>),
    /// A source position or rate cannot be rendered.
    #[error("projected activation is not a finite forward source span")]
    InvalidCoordinate,
}

/// One immutable projected map and its exact pending activation boundary.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct WarpPlan {
    #[field(get, copy)]
    activation: WarpCursor,
    map: WarpMap,
}

impl WarpPlan {
    /// Prepares a projected activation without changing any active renderer.
    ///
    /// # Errors
    /// Returns the original geometry refusal or an unrepresentable coordinate.
    pub fn new(map: WarpMap, output: SessionFrame) -> Result<Self, WarpPlanError> {
        let source = match map.source_at(output) {
            BeatGridQuery::Resolved(source) => f64::from(source)
                .round()
                .to_u64()
                .ok_or(WarpPlanError::InvalidCoordinate)?,
            refusal => return Err(WarpPlanError::Source(refusal)),
        };
        match map.rate_at(output) {
            BeatGridQuery::Resolved(rate) if rate.is_finite() && rate > 0.0 => {}
            BeatGridQuery::Resolved(_) => return Err(WarpPlanError::InvalidCoordinate),
            refusal => return Err(WarpPlanError::Rate(refusal)),
        }
        let activation = map.reanchor(source, output);
        Ok(Self { activation, map })
    }

    delegate::delegate! {
        to self.map {
            /// Absolute source endpoint on the map's session axis.
            pub fn source_at(&self, output: SessionFrame) -> BeatGridQuery<AssetFrame>;
            /// Tempo of the target trajectory frozen in this applied plan.
            pub fn target_tempo_at(&self, output: SessionFrame) -> BeatGridQuery<crate::BeatsPerMinute>;
            /// Beat of the target trajectory frozen in this applied plan.
            pub fn target_beat_at(&self, output: SessionFrame) -> BeatGridQuery<crate::BeatEstimate<crate::MapPoint<crate::Beat>>>;
            /// Meter carried by a beat on the plan's frozen target grid.
            pub fn target_meter_at(&self, beat: crate::MapPoint<crate::Beat>) -> BeatGridQuery<crate::BeatEstimate<crate::Meter>>;
            /// Source frames per session output frame, including sample rates.
            pub fn rate_at(&self, output: SessionFrame) -> BeatGridQuery<f64>;
            /// Source axis carried by this immutable map.
            #[must_use]
            pub fn source_axis(&self) -> Option<MapAxis>;
            /// Session axis carried by this immutable map.
            #[must_use]
            pub fn output_axis(&self) -> Option<MapAxis>;
        }
    }
}

/// Whole-plan publication to one resident renderer. Readers retain their active
/// predecessor until the published plan reaches its activation boundary.
#[derive(Debug, Default)]
pub struct WarpPlanSlot {
    plan: ArcSwapOption<WarpPlan>,
}

impl WarpPlanSlot {
    delegate::delegate! {
        to self.plan {
            /// Reads a resident plan on the worker side, outside the audio callback.
            #[must_use]
            #[call(load_full)]
            pub fn load(&self) -> Option<Arc<WarpPlan>>;
            /// Publishes a prepared immutable plan, or restores manual selection.
            #[call(store)]
            pub fn install(&self, plan: Option<Arc<WarpPlan>>);
        }
    }
}
