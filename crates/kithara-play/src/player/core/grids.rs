use kithara_platform::sync::Arc;
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridRevision, BeatGridSnapshot, BeatGridState, ReconcileCause,
    SegmentSet, SyncAdmission, SyncApplied, SyncError, SyncGroup, SyncMember, SyncOperation,
    SyncStatusSnapshot, TopologyOperation,
};
use tracing::warn;

use super::PlayerImpl;
use crate::{
    api::TrackId,
    player::{protocol::PlayerMember, state::TrackGrid},
};

/// One published asset grid of a queued track, boxed as a topology member.
struct TrackGridMember(BeatGridSnapshot);

impl BeatGrid for TrackGridMember {
    delegate::delegate! {
        to self.0 {
            fn id(&self) -> BeatGridId;
            #[call(clone)]
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl<S> PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    /// Publishes the asset grid of one queued track on this deck's sync group
    /// and reconciles the track onto the deck.
    ///
    /// A first publication allocates the grid identity and attaches it; a later
    /// one replaces the member under the next revision.
    ///
    /// # Errors
    ///
    /// Returns the group's rejection or the exhausted identity space.
    pub(crate) fn publish_item_grid(
        &mut self,
        item: TrackId,
        segments: SegmentSet,
        state: BeatGridState,
    ) -> Result<SyncAdmission, SyncError> {
        let previous = self.runtime.core.items.track_grid(item);
        let (id, revision, cause) = match &previous {
            Some(grid) => (
                grid.id,
                grid.revision
                    .checked_next()
                    .ok_or(SyncError::BeatGridRevisionExhausted { grid_id: grid.id })?,
                ReconcileCause::GridRefined,
            ),
            None => (
                BeatGridId::allocate()?,
                BeatGridRevision::first(),
                ReconcileCause::GridAvailable,
            ),
        };
        let snapshot = BeatGridSnapshot::segments(id, revision, state, segments.clone())?;
        let base = self.sync.topology()?.stamp();
        let member = SyncMember::Grid {
            alignment: None,
            grid: Box::new(TrackGridMember(snapshot)),
        };
        let operation = if previous.is_some() {
            TopologyOperation::Replace {
                member: id,
                replacement: member,
            }
        } else {
            TopologyOperation::Attach { member }
        };
        let _ = self.transact_sync(SyncOperation::Topology {
            base,
            operations: Box::new([operation]),
        })?;
        self.runtime.core.items.publish_track_grid(
            item,
            TrackGrid {
                id,
                revision,
                segments,
            },
        );
        let sync = &self.sync;
        #[cfg(target_arch = "wasm32")]
        let sync = sync.owned()?;
        self.replan_track(item);
        let (load, transport) = sync.generations();
        let frontier = self.runtime.presentation_frontier();
        self.transact_sync(SyncOperation::Reconcile {
            target: id,
            load,
            transport,
            cause,
            frontier,
        })
    }

    /// Acknowledges the prepared warp map once the deck's presentation
    /// frontier has reached its activation frame.
    ///
    /// # Errors
    ///
    /// Returns the group's acknowledgement error.
    pub(crate) fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError> {
        let sync = &self.sync;
        #[cfg(target_arch = "wasm32")]
        let sync = sync.owned()?;
        let Some(prepared) = sync.prepared() else {
            return Ok(None);
        };
        let frontier = self.runtime.presentation_frontier();
        if frontier.output() < prepared.activation {
            return Ok(None);
        }
        let (load, transport) = sync.generations();
        let topology = self.sync.topology()?.stamp();
        let applied = SyncApplied::builder()
            .group(self.sync.snapshot().stamp())
            .load(load)
            .frontier(frontier)
            .operation(prepared.operation)
            .topology(topology)
            .transport(transport)
            .warp_map(prepared.warp_map)
            .build();
        self.sync.acknowledge(applied).map(Some)
    }

    fn replan_track(&self, item: TrackId) {
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return;
        };
        let plan = grid
            .segments
            .region_plan()
            .inspect_err(|error| warn!(%error, %item, "track grid has no region plan"))
            .ok();
        self.runtime
            .core
            .items
            .set_track_plan(item, plan.map(Arc::new));
    }

    fn transact_sync(
        &mut self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncError> {
        self.sync.transact(operation).map_err(|rejected| {
            let (error, _): (SyncError, SyncOperation<PlayerMember>) = rejected.into();
            error
        })
    }
}
