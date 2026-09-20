use std::num::NonZeroU32;

use num_traits::ToPrimitive;

use crate::{
    AssetAxis, AssetFrame, Beat, BeatEvidence, BeatGridId, BeatGridRevision, BeatGridSnapshot,
    BeatGridState, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis, MapPosition, MapSegment,
    SegmentFacts, SegmentSet, SessionAnchor, SessionAxis, SessionBeat, SessionEpoch, SessionFrame,
    WarpPlan,
};

/// Beats a fixture recording is analysed over.
pub(crate) const BEATS: i64 = 400;
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Frames one beat of `bpm` occupies at `sample_rate`.
pub(crate) fn beat_frames(bpm: f64, sample_rate: NonZeroU32) -> f64 {
    f64::from(sample_rate.get()) * SECONDS_PER_MINUTE / bpm
}

/// One analysed recording at a steady `bpm`, on its own asset axis.
pub(crate) fn asset_grid(bpm: f64, sample_rate: NonZeroU32) -> BeatGridSnapshot {
    let last = beat_frames(bpm, sample_rate) * BEATS.to_f64().unwrap_or_default();
    let marker = |frame: f64, ordinal: i64| {
        BeatMarker::new(
            MapPosition::Asset(AssetFrame::new(frame).expect("invariant: fixture frame is finite")),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::ZERO,
        )
    };
    let segment = MapSegment::new(
        marker(0.0, 0),
        marker(last, BEATS),
        SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
    )
    .expect("invariant: fixture markers form an increasing relation");
    let axis = AssetAxis::new(sample_rate, last.ceil().to_u64().unwrap_or_default() + 1);
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
pub(crate) fn session_grid(bpm: f64, sample_rate: NonZeroU32) -> BeatGridSnapshot {
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::new(0.0).expect("invariant: fixture beat is finite"),
        bpm / SECONDS_PER_MINUTE,
        SessionAxis::new(sample_rate, SessionEpoch::new(0)),
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

/// A plan carrying one recording projected onto a host running at `host_bpm`.
pub(crate) fn projected_plan(source_bpm: f64, host_bpm: f64, sample_rate: NonZeroU32) -> WarpPlan {
    let projection = BeatGridSnapshot::projection(
        asset_grid(source_bpm, sample_rate),
        session_grid(host_bpm, sample_rate),
        Beat::new(0.0).expect("invariant: fixture cue is finite"),
        SessionFrame::new(0),
    )
    .expect("invariant: an asset grid projects onto a live session grid");
    WarpPlan::new(projection)
}

/// One analysed recording whose beats are spaced by frame count, laid on an
/// axis of `frames`.
///
/// Each entry is `(start_frame, frames_per_beat, beats)`. Spacing states the
/// tempo exactly, which a beats-per-second figure cannot always do, so a
/// fixture can place a beat on a chosen frame and mean it.
///
/// A pass that marked only part of a track leaves segments that stop short of
/// the axis the track declares, and the set answers the rest by extending
/// them. Stating `frames` apart from the marked span is how a fixture asks for
/// that; `None` ends the axis one frame past the last marked beat.
pub(crate) fn asset_grid_over(
    spans: &[(f64, f64, i64)],
    frames: Option<u64>,
    sample_rate: NonZeroU32,
) -> BeatGridSnapshot {
    let marker = |frame: f64, ordinal: i64| {
        BeatMarker::new(
            MapPosition::Asset(AssetFrame::new(frame).expect("invariant: fixture frame is finite")),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::ZERO,
        )
    };
    let mut ordinal = 0;
    let mut end = 0.0;
    let mut segments = Vec::with_capacity(spans.len());
    for (start, frames_per_beat, beats) in spans {
        end = beats
            .to_f64()
            .unwrap_or_default()
            .mul_add(*frames_per_beat, *start);
        segments.push(
            MapSegment::new(
                marker(*start, ordinal),
                marker(end, ordinal + beats),
                SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
            )
            .expect("invariant: fixture markers form an increasing relation"),
        );
        ordinal += beats;
    }
    let axis = AssetAxis::new(
        sample_rate,
        frames.unwrap_or_else(|| end.ceil().to_u64().unwrap_or_default() + 1),
    );
    BeatGridSnapshot::segments(
        BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
        BeatGridRevision::first(),
        BeatGridState::Complete,
        SegmentSet::new(MapAxis::Asset(axis), segments)
            .expect("invariant: fixture segments are valid"),
    )
    .expect("invariant: complete geometry is valid on the asset axis")
}

/// A live session grid whose beats are spaced by frame count.
pub(crate) fn session_grid_spaced(
    frames_per_beat: f64,
    sample_rate: NonZeroU32,
) -> BeatGridSnapshot {
    session_grid(
        f64::from(sample_rate.get()) * SECONDS_PER_MINUTE / frames_per_beat,
        sample_rate,
    )
}

/// A plan carrying a spaced recording projected onto a spaced host grid.
pub(crate) fn spaced_plan(
    spans: &[(f64, f64, i64)],
    host_frames_per_beat: f64,
    sample_rate: NonZeroU32,
) -> WarpPlan {
    let projection = BeatGridSnapshot::projection(
        asset_grid_over(spans, None, sample_rate),
        session_grid_spaced(host_frames_per_beat, sample_rate),
        Beat::new(0.0).expect("invariant: fixture cue is finite"),
        SessionFrame::new(0),
    )
    .expect("invariant: an asset grid projects onto a live session grid");
    WarpPlan::new(projection)
}
