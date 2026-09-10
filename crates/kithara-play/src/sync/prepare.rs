use kithara_warp::{
    AssetFrame, Beat, BeatAlignment, BeatGridQuery, BeatGridSnapshot, BeatGridState, MapPoint,
    MapPosition, MapRegion, PresentationFrontier, SessionFrame, SyncOperationId, WarpMapRevision,
};
use num_traits::ToPrimitive;

/// A warp map admitted for one grid member and awaiting the renderer's
/// acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedSync {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) activation: SessionFrame,
}

/// The beat alignment of one grid member onto its owner's grid and the
/// owner-grid frame on which it becomes audible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MemberAlignment {
    pub(super) alignment: BeatAlignment,
    pub(super) activation: SessionFrame,
}

/// Aligns the member's beat under the frontier's source frame onto the next
/// whole owner beat after the frontier's output frame.
///
/// Returns the region whose geometry is still missing when either grid cannot
/// answer.
pub(super) fn align_member(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    frontier: PresentationFrontier,
) -> Result<MemberAlignment, MapRegion> {
    let source = MapPosition::Asset(
        frontier
            .source()
            .to_f64()
            .and_then(|frame| AssetFrame::new(frame).ok())
            .unwrap_or_default(),
    );
    let output = MapPosition::Session(frontier.output());
    if member.state() != BeatGridState::Complete {
        return Err(MapRegion::point(source));
    }
    let member_origin = MapPoint::new(member.stamp(), source);
    let BeatGridQuery::Resolved(_) = member.tempo_at(member_origin) else {
        return Err(MapRegion::point(source));
    };
    let BeatGridQuery::Resolved(member_beat) = member.beat_at(member_origin) else {
        return Err(MapRegion::point(source));
    };
    let BeatGridQuery::Resolved(owner_beat) = owner.beat_at(MapPoint::new(owner.stamp(), output))
    else {
        return Err(MapRegion::point(output));
    };
    let member_beat =
        whole_beat(*member_beat.value().value()).ok_or_else(|| MapRegion::point(source))?;
    let owner_beat =
        whole_beat(*owner_beat.value().value()).ok_or_else(|| MapRegion::point(output))?;
    let target = MapPoint::new(owner.stamp(), owner_beat);
    let BeatGridQuery::Resolved(position) = owner.position_at(target) else {
        return Err(MapRegion::point(output));
    };
    let MapPosition::Session(activation) = *position.value().value() else {
        return Err(MapRegion::point(output));
    };
    Ok(MemberAlignment {
        alignment: BeatAlignment::new(MapPoint::new(member.stamp(), member_beat), target),
        activation,
    })
}

fn whole_beat(beat: Beat) -> Option<Beat> {
    Beat::new(f64::from(beat).ceil()).ok()
}
