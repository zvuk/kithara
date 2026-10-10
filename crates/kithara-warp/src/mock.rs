use std::num::NonZeroU32;

use kithara_signal::{SessionEpoch, SessionFrame};
use num_traits::ToPrimitive;

use crate::{
    AssetAxis, AssetExtent, AssetFrame, BeatEvidence, BeatGridId, BeatGridRevision,
    BeatGridSnapshot, BeatGridState, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis,
    MapPosition, MapSegment, SegmentFacts, SegmentSet, SessionAnchor, SessionBeat,
};

mod consts {
    pub(super) const SECONDS_PER_MINUTE: f64 = 60.0;
}

/// Beats a fixture recording is analysed over.
pub const BEATS: i64 = 400;

/// Frames one beat of `bpm` occupies at `sample_rate`.
#[must_use]
pub fn beat_frames(bpm: f64, sample_rate: NonZeroU32) -> f64 {
    f64::from(sample_rate.get()) * consts::SECONDS_PER_MINUTE / bpm
}

/// One analysed recording at a steady `bpm`, on its own asset axis.
///
/// # Panics
///
/// Panics when `bpm` does not give finite, increasing beat frames.
#[must_use]
pub fn asset_grid(bpm: f64, sample_rate: NonZeroU32) -> BeatGridSnapshot {
    let last = beat_frames(bpm, sample_rate) * BEATS.to_f64().unwrap_or_default();
    let marker = |frame: f64, ordinal: i64| {
        BeatMarker::new(
            MapPosition::Asset(AssetFrame::new(frame).expect("invariant: fixture frame is finite")),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::new(0.0).expect("zero uncertainty"),
        )
    };
    let segment = MapSegment::new(
        marker(0.0, 0),
        marker(last, BEATS),
        SegmentFacts::new(
            BeatEvidence::Observed,
            FrameUncertainty::new(0.0).expect("zero uncertainty"),
            None,
        ),
    )
    .expect("invariant: fixture markers form an increasing relation");
    let axis = AssetAxis::new(
        sample_rate,
        AssetExtent::Bounded(last.ceil().to_u64().unwrap_or_default() + 1),
    );
    BeatGridSnapshot::segments(
        BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
        BeatGridRevision::first(),
        BeatGridState::Complete,
        SegmentSet::new(MapAxis::Asset(axis), vec![segment])
            .expect("invariant: fixture segments are valid"),
    )
    .expect("invariant: complete geometry is valid on the asset axis")
}

/// One live session grid running at a steady `bpm`.
///
/// # Panics
///
/// Panics when `bpm` is not a finite, invertible tempo.
#[must_use]
pub fn session_grid(bpm: f64, sample_rate: NonZeroU32) -> BeatGridSnapshot {
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("invariant: fixture beat is finite"),
        bpm / consts::SECONDS_PER_MINUTE,
        sample_rate,
    )
    .expect("invariant: fixture tempo is invertible");
    BeatGridSnapshot::session(
        BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    )
}
