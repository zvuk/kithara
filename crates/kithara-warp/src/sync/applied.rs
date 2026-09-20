use super::{
    LoadGeneration, PresentationFrontier, SyncOperationId, TopologyStamp, TransportRevision,
    WarpMapRevision,
};
use crate::BeatGridStamp;

/// An audible acknowledgement tied to every admission axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, bon::Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncApplied {
    /// Exact group-grid identity and revision used by the warp map.
    #[field(get, copy)]
    group: BeatGridStamp,
    /// Track load for which the warp map was admitted.
    #[field(get, copy)]
    load: LoadGeneration,
    /// Actual source/output frontier consumed by the callback.
    #[field(get, copy)]
    frontier: PresentationFrontier,
    /// Synchronization operation whose activation became audible.
    #[field(get, copy)]
    operation: SyncOperationId,
    /// Exact ownership tree used by the warp map.
    #[field(get, copy)]
    topology: TopologyStamp,
    /// Exact session transport state used by the warp map.
    #[field(get, copy)]
    transport: TransportRevision,
    /// Exact immutable warp map consumed by the callback.
    #[field(get, copy)]
    warp_map: WarpMapRevision,
}
