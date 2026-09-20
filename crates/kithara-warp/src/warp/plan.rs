use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;

use super::WarpCursor;
use crate::{AssetFrame, BeatGridQuery, BeatGridSnapshot, MapPoint, RateTarget, SessionFrame};

/// Immutable manual target paired with one Free map activation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeActivation {
    cursor: WarpCursor,
    rate: RateTarget,
}

impl FreeActivation {
    #[must_use]
    pub const fn new(cursor: WarpCursor, rate: RateTarget) -> Self {
        Self { cursor, rate }
    }

    #[must_use]
    pub const fn cursor(self) -> WarpCursor {
        self.cursor
    }

    #[must_use]
    pub const fn rate(self) -> RateTarget {
        self.rate
    }
}

/// One item's beat geometry as the renderer consumes it.
///
/// The plan carries a grid rather than a tempo per span, so the renderer asks
/// it where the recording stands at an output frame and takes the difference
/// between two such answers as the span to consume. Nothing in it accumulates,
/// because every answer is absolute.
///
/// A deck that follows a grid installs the projection onto that grid. A deck
/// that follows nothing installs the recording's own grid, which answers
/// output frames with a refusal, and the recording is then heard untouched.
#[derive(Debug, Clone, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct WarpPlan {
    /// Exact source/output relation prepared for this plan.
    #[field(with, option_set_some, get, copy)]
    activation: Option<WarpCursor>,
    /// Free activation context paired atomically with its map cursor.
    #[field(get, copy)]
    free_activation: Option<FreeActivation>,
    /// The grid every span is measured against.
    grid: BeatGridSnapshot,
}

impl WarpPlan {
    /// Builds the plan one grid prescribes.
    #[must_use]
    pub const fn new(grid: BeatGridSnapshot) -> Self {
        Self {
            activation: None,
            free_activation: None,
            grid,
        }
    }

    /// Resolves the recording frame that sounds at the output frame `output`.
    ///
    /// The stamp is the plan's own, because the renderer counts output frames
    /// itself rather than carrying a coordinate from an older grid. A grid the
    /// projection no longer describes is refused upstream, where its owner
    /// rebuilds the plan against the moved grid.
    pub fn source_at(&self, output: SessionFrame) -> BeatGridQuery<AssetFrame> {
        self.grid
            .source_at(MapPoint::new(self.grid.stamp(), output.into()))
    }

    /// Resolves the rate the recording plays at when heard at `output`.
    ///
    /// The renderer hands this to the stretch backend. It carries no phase:
    /// the phase lives in the difference between two [`Self::source_at`]
    /// answers, which is why a rounding error here cannot accumulate.
    pub fn rate_at(&self, output: SessionFrame) -> BeatGridQuery<f64> {
        self.grid
            .rate_at(MapPoint::new(self.grid.stamp(), output.into()))
    }

    /// Whether the plan places this item on the output axis.
    ///
    /// A deck that follows a grid installs a projection, whose axis is the
    /// session. A deck that follows nothing installs the recording's own grid,
    /// whose axis is the asset: it answers no output frame, and the recording
    /// is heard exactly as recorded.
    #[must_use]
    pub fn follows_output(&self) -> bool {
        matches!(self.grid.axis(), crate::MapAxis::Session(_))
    }

    /// Pairs a Free activation with the map cursor it belongs to.
    ///
    /// The cursor is the plan's activation as well: a Free handoff installs a
    /// map at that exact source frame, and a renderer that saw only the manual
    /// target would consume the discontinuity without applying the map.
    #[must_use]
    pub fn with_free_activation(mut self, free: FreeActivation) -> Self {
        self.activation = Some(free.cursor());
        self.free_activation = Some(free);
        self
    }
}

/// Live plan of one resident item, handed from the deck to the renderer that
/// stretches that item. Swapped whole; picked up on the next chunk.
#[derive(Debug, Default)]
pub struct WarpPlanSlot {
    plan: ArcSwapOption<WarpPlan>,
}

impl WarpPlanSlot {
    /// Exact activation carried by the installed plan, if it has one.
    #[must_use]
    pub fn activation(&self) -> Option<WarpCursor> {
        self.load().as_deref().and_then(WarpPlan::activation)
    }

    delegate::delegate! {
        to self.plan {
            /// The installed plan, if any.
            #[must_use]
            #[call(load_full)]
            pub fn load(&self) -> Option<Arc<WarpPlan>>;
            /// Install or clear the plan.
            #[call(store)]
            pub fn install(&self, plan: Option<Arc<WarpPlan>>);
        }
    }
}
