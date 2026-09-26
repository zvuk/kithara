use kithara_signal::{SessionFrame, TransportRevision};
use kithara_warp::{AssetFrame, BeatAlignment, BeatGridStamp, WarpMapRevision, WarpPlan};

use crate::{LoadGeneration, SyncOperationId, TopologyStamp};

/// Every fact one preparation was calculated against.
///
/// An executor may carry the preparation out only while each of these still
/// holds; a change to any of them makes the preparation stale rather than
/// adjustable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncExecutionStamp {
    /// Operation that produced the preparation.
    #[field(get, copy)]
    operation: SyncOperationId,
    /// Exact member grid the preparation carries.
    #[field(get, copy)]
    member: BeatGridStamp,
    /// Exact group grid the member follows.
    #[field(get, copy)]
    group: BeatGridStamp,
    /// Exact ownership tree the preparation was admitted against.
    #[field(get, copy)]
    topology: TopologyStamp,
    /// Track load the preparation belongs to.
    #[field(get, copy)]
    load: LoadGeneration,
    /// Session transport state the preparation was calculated for.
    #[field(get, copy)]
    transport: TransportRevision,
    /// Processed output revision this Host-dependent plan requires, if any.
    #[field(get, copy, with(vis = "pub(crate)"))]
    output_transport: Option<TransportRevision>,
}

/// What one preparation asks its executor to do.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SyncEffect {
    /// Carry a member's recording onto the group's beats from the plan's
    /// exact activation.
    Projection {
        /// The member beat and the group beat that sound together.
        alignment: BeatAlignment,
        /// The projected map and the boundary at which it takes over.
        plan: WarpPlan,
        /// The applied map the plan takes over from; `None` when the member
        /// starts sounding with it.
        replaces: Option<WarpMapRevision>,
    },
    /// Release a member from the group's beats: from `activation` its
    /// recording continues unsynchronized from `source`, where the applied
    /// map had carried it.
    Handoff {
        /// The applied map the member leaves.
        replaces: WarpMapRevision,
        /// The recording frame the applied map reaches at `activation`.
        source: AssetFrame,
        /// The session frame at which the member leaves the map.
        activation: SessionFrame,
    },
}

/// One member's immutable, not yet applied synchronization decision.
///
/// A preparation authorizes nothing by itself: the caller decides whether and
/// when the member launches, and the group learns the outcome only through an
/// acknowledgement.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncPreparation {
    /// Exact facts the decision holds for.
    #[field(get, copy)]
    stamp: SyncExecutionStamp,
    /// Effect the executor carries out.
    #[field(get)]
    effect: SyncEffect,
}

impl SyncPreparation {
    pub(crate) const fn new(stamp: SyncExecutionStamp, effect: SyncEffect) -> Self {
        Self { stamp, effect }
    }

    /// The map the effect renders from its activation on, `None` for a
    /// handoff, and the session boundary at which it takes over.
    pub(crate) fn activation(&self) -> (Option<WarpMapRevision>, SessionFrame) {
        match &self.effect {
            SyncEffect::Projection { plan, .. } => (
                Some(plan.activation().revision()),
                plan.activation().output(),
            ),
            SyncEffect::Handoff { activation, .. } => (None, *activation),
        }
    }
}

impl SyncExecutionStamp {
    pub(crate) const fn new(
        operation: SyncOperationId,
        member: BeatGridStamp,
        group: BeatGridStamp,
        topology: TopologyStamp,
        load: LoadGeneration,
        transport: TransportRevision,
    ) -> Self {
        Self {
            operation,
            member,
            group,
            topology,
            load,
            transport,
            output_transport: None,
        }
    }
}
