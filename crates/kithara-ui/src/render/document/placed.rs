use crate::{draw::Pt, expand::Binding, ids::InternId};

/// One placement of a stage, as its host mounts it.
///
/// The point is where the child's box goes inside the stage, not an offset on
/// what it draws, so the region that answers the pointer travels with it. A
/// placement with somewhere to write may be carried; one without stands where
/// the document puts it.
#[non_exhaustive]
pub struct PlacedMount<'a> {
    pub path: InternId,
    /// Where the point comes from, for a host that re-reads endpoints into a
    /// tree it keeps rather than mounting the document again.
    pub read: Option<&'a Binding>,
    pub snap: Option<Snap>,
    pub write: Option<&'a Binding>,
    pub at: Pt,
}

/// What takes a carried placement: the points of the placements its magnet
/// names, and how near it must come before one of them does.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Snap {
    pub to: Vec<Pt>,
    pub within: f32,
}

impl Snap {
    /// Where a drag ends: the nearest point in reach, or where the pointer left
    /// it. Both hosts publish through this, so a magnet answers once.
    #[must_use]
    pub fn take(&self, at: Pt) -> Pt {
        self.to
            .iter()
            .copied()
            .filter(|target| target.distance(at) <= self.within)
            .min_by(|one, other| one.distance(at).total_cmp(&other.distance(at)))
            .unwrap_or(at)
    }
}
