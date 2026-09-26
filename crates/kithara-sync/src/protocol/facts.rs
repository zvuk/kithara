use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_warp::{BeatGridStamp, MeterFacts, SessionAnchor, SessionAxis};

/// One timeline fact a group passes on to each of its direct child groups.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ParentFact {
    /// The parent's beat timeline moved onto a new segment.
    Segment(ParentGridUpdate),
    /// The parent's beat timeline lost its geometry.
    Withdrawn(ParentWithdrawal),
    /// The physical session axis changed.
    Axis(SessionAxisUpdate),
    /// The group became a direct child of a parent on this session axis.
    ///
    /// The group missed every route boundary its new parent crossed before,
    /// so it drops its own axis instead of stepping through a successor.
    Joined(SessionAxisUpdate),
}

/// A parent's accepted tempo and phase segment, offered to one direct child.
///
/// Only a child in [`crate::SyncMode::HostSync`] adopts it; a child in any
/// other mode records it so a later enable follows the parent's current
/// segment rather than one it never saw.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct ParentGridUpdate {
    /// Returns the parent grid identity and revision this segment belongs to.
    #[field(get, copy)]
    parent: BeatGridStamp,
    /// Returns the session-axis generation the anchor's frames are on.
    #[field(get, copy)]
    epoch: SessionEpoch,
    /// Returns the parent's tempo trajectory from its commit frame onwards.
    #[field(get, copy)]
    anchor: SessionAnchor,
    /// Returns the parent's meter evidence, when it has any.
    #[field(get, copy)]
    meter: Option<MeterFacts>,
    /// Processed output revision inherited only by Host-following children.
    #[field(get, copy, with, option_set_some)]
    output_transport: Option<TransportRevision>,
    /// Current processed output end, used only as a fresh execution floor.
    #[field(get, copy, with, option_set_some)]
    execution_floor: Option<SessionFrame>,
}

impl ParentGridUpdate {
    /// Describes one parent segment on its exact session axis.
    #[must_use]
    pub const fn new(
        parent: BeatGridStamp,
        epoch: SessionEpoch,
        anchor: SessionAnchor,
        meter: Option<MeterFacts>,
    ) -> Self {
        Self {
            parent,
            epoch,
            anchor,
            meter,
            output_transport: None,
            execution_floor: None,
        }
    }

    /// Returns the session axis the segment's frames are measured on.
    #[must_use]
    pub fn axis(self) -> SessionAxis {
        SessionAxis::new(self.anchor.sample_rate(), self.epoch)
    }
}

/// The physical session axis changed: a new epoch, possibly at a new rate.
///
/// Every mode accepts it, because every frame planned on the previous axis is
/// meaningless on the new one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SessionAxisUpdate {
    /// Returns the axis every later frame is measured on.
    #[field(get, copy)]
    axis: SessionAxis,
}

impl SessionAxisUpdate {
    /// Announces the session axis that replaces the current one.
    #[must_use]
    pub const fn new(axis: SessionAxis) -> Self {
        Self { axis }
    }
}

/// A parent's beat timeline has no geometry from `at` on.
///
/// A child in [`crate::SyncMode::HostSync`] withdraws its own grid with it
/// and, when the parent released its sounding members, releases its own
/// under the same transport; a child in any other mode only records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct ParentWithdrawal {
    /// Returns the parent grid identity and revision that has no geometry.
    #[field(get, copy)]
    parent: BeatGridStamp,
    /// Returns the session frame from which the timeline is withdrawn.
    #[field(get, copy)]
    at: SessionFrame,
    /// Returns the transport under which sounding members are released, or
    /// `None` when they keep their applied maps.
    #[field(get, copy)]
    release: Option<TransportRevision>,
}

impl ParentWithdrawal {
    /// Describes one parent timeline withdrawn at an exact session frame.
    #[must_use]
    pub const fn new(
        parent: BeatGridStamp,
        at: SessionFrame,
        release: Option<TransportRevision>,
    ) -> Self {
        Self {
            parent,
            at,
            release,
        }
    }
}
