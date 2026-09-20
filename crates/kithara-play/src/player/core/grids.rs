use std::num::NonZeroU32;

use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
use kithara_warp::{
    AlignmentSource, AssetFrame, Beat, BeatGrid, BeatGridId, BeatGridQuery, BeatGridRevision,
    BeatGridSnapshot, BeatGridState, MapAxis, MapPoint, MapPosition, MemberArm,
    PresentationFrontier, ReconcileCause, SegmentSet, SyncAdmission, SyncApplied, SyncError,
    SyncGroup, SyncMember, SyncOperation, SyncRejected, SyncStatusSnapshot, TopologyOperation,
    WarpPlan,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::PlayerImpl;
use crate::{
    api::TrackId,
    bridge::{PreparedLaunchIdentity, channels::ScheduledSeekReanchor},
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
    /// one replaces the member under the next revision. A queued track the deck
    /// does not play yet stops there and its admission is the topology change,
    /// because only the deck's entry preparation places a waiting member.
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
        let snapshot = BeatGridSnapshot::segments(id, revision, state, segments)?;
        let base = self.sync.topology()?.stamp();
        let member = SyncMember::Grid {
            alignment: None,
            arm: MemberArm::Waiting,
            grid: Box::new(TrackGridMember(snapshot.clone())),
        };
        let operation = if previous.is_some() {
            TopologyOperation::Replace {
                member: id,
                replacement: member,
            }
        } else {
            TopologyOperation::Attach { member }
        };
        let attached = self.transact_sync(SyncOperation::Topology {
            base,
            operations: Box::new([operation]),
        })?;
        self.runtime.core.items.publish_track_grid(
            item,
            TrackGrid {
                id,
                revision,
                snapshot: snapshot.clone(),
            },
        );
        self.replan_track(item);
        self.runtime.retire_presented_source_cue(item);
        if self.runtime.core.items.current_item_id() != Some(item) {
            return Ok(attached);
        }
        if self.sync.mode() == kithara_warp::SyncMode::HostSync
            && self.runtime.presentation_frontier_for(item, None).is_none()
        {
            return Ok(attached);
        }
        let (prepared_launch, source_cue) = self.await_prepared_launch(item, &snapshot);
        self.reconcile_item_grid(item, cause, None, prepared_launch, source_cue)
            .map_err(|rejected| {
                let (error, _) = rejected.into();
                error
            })?
            .ok_or_else(|| SyncError::MemberNotFound {
                group_id: self.sync.id(),
                member_id: id,
            })
    }

    /// Reconciles the deck's own track onto the group and carries the map the
    /// group prepares into the renderer.
    ///
    /// The deck owns the track it holds, so the member is armed here: only a
    /// queued track that has yet to reach the deck enters through a prepared
    /// entry instead.
    pub(super) fn reconcile_item_grid(
        &mut self,
        item: TrackId,
        cause: ReconcileCause,
        source: Option<AlignmentSource>,
        prepared_launch: bool,
        source_cue: Option<Beat>,
    ) -> Result<Option<SyncAdmission>, SyncRejected<PlayerMember>> {
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return Ok(None);
        };
        let output_rate = self.runtime.core.engine.output_sample_rate();
        let axis = grid.snapshot.axis();
        let observed = self.runtime.presentation_frontier_for(item, None);
        let source = source.unwrap_or_else(|| {
            let frontier = observed.map_or_else(
                || {
                    PresentationFrontier::builder()
                        .output(kithara_warp::SessionFrame::new(0))
                        .source(0)
                        .build()
                },
                |frontier| frontier.on_axis(axis, output_rate),
            );
            self.runtime.playback_snapshot().map_or(
                AlignmentSource::Prepared(frontier),
                |snapshot| {
                    if snapshot.is_playing() {
                        AlignmentSource::Audible {
                            presentation: frontier,
                            preparation_source: native_frame(
                                snapshot.preparation_source(
                                    observed.map_or(0, |frontier| frontier.source()),
                                    self.runtime.core.response_budget_frames,
                                ),
                                axis,
                                output_rate,
                            ),
                            playback_rate: kithara_warp::RateTarget::default()
                                .with_speed(snapshot.rate),
                        }
                    } else {
                        AlignmentSource::Prepared(frontier)
                    }
                },
            )
        });
        self.sync.arm_deck_track(grid.id);
        let (load, transport) = {
            let sync = &self.sync;
            sync.generations()
        };
        let admission = self.sync.transact(SyncOperation::Reconcile {
            target: grid.id,
            load,
            transport,
            cause,
            source,
            source_cue,
        })?;
        let prepared_source_cue = prepared_launch
            && source_cue.is_some()
            && matches!(admission, SyncAdmission::Prepared { .. });
        let prepared = {
            let sync = &self.sync;
            sync.prepared().get(grid.id)
        };
        if let Some(prepared) = prepared {
            kithara::probe_event!(
                warp_plan_published,
                warp_map_revision = u64::from(prepared.warp_map),
                presentation_source = source.frontier().source(),
                preparation_source = source.preparation_source(),
                activation_source = prepared.source,
                activation_output = i64::from(prepared.activation)
            );
            self.deliver_prepared_map(item, &prepared, prepared_launch);
            if prepared_source_cue && let Some(slot) = self.runtime.slot() {
                self.arm_prepared_launch_while_playing(slot, item);
            }
        }
        Ok(Some(admission))
    }

    /// Arms a scheduled prepared launch while the player is playing.
    ///
    /// The armed launch owns the track's initial source cue, so the cue is
    /// consumed only once the launch accepts the arm.
    pub(crate) fn arm_prepared_launch_while_playing(
        &mut self,
        slot: crate::api::SlotId,
        item: TrackId,
    ) {
        if self.runtime.phase_kind() == crate::player::state::phase::PlayerPhaseKind::Playing
            && self
                .runtime
                .core
                .engine
                .set_prepared_launch_armed(slot, item, true)
        {
            self.runtime
                .core
                .items
                .consume_awaiting_initial_source_cue(item);
        }
    }

    /// Acknowledges the prepared warp map once matching PCM reaches presentation.
    ///
    /// # Errors
    ///
    /// Returns the group's acknowledgement error.
    pub(crate) fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError> {
        self.adopt_free_receipt();
        let sync = &self.sync;
        let Some(item) = self.runtime.core.items.current_item_id() else {
            return Ok(None);
        };
        let Some(prepared) = self
            .runtime
            .core
            .items
            .track_grid(item)
            .and_then(|grid| sync.prepared().get(grid.id))
        else {
            return Ok(None);
        };
        let free = prepared.frees_deck();
        let Some(frontier) = self
            .runtime
            .presentation_frontier_for(item, Some(prepared.warp_map))
        else {
            return Ok(None);
        };
        if !prepared_is_presented(frontier, prepared.activation, prepared.warp_map) {
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
        let status = self.sync.acknowledge(applied)?;
        kithara::probe_event!(
            prepared_sync_acknowledged,
            free = if free { 1_u64 } else { 0 },
            warp_map_revision = u64::from(prepared.warp_map),
            activation_output = i64::from(prepared.activation)
        );
        Ok(Some(status))
    }

    pub(crate) fn prepare_free_handoff(&self) {
        let (Some(item), Some(prepared)) = (
            self.runtime.core.items.current_item_id(),
            self.sync.preparing(),
        ) else {
            return;
        };
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return;
        };
        let plan = Arc::new(WarpPlan::new(grid.snapshot.clone()));
        let published = self.runtime.core.items.publish_free_adoption(
            item,
            crate::worker::FreeAdoptionRequest {
                stamp: prepared.stamp,
                alignment: prepared.alignment,
                item,
                decode_epoch: 0,
                manual_rate: prepared.manual_rate,
                plan,
            },
        );
        if published {
            self.runtime.core.worker.wake();
        }
    }

    fn adopt_free_receipt(&mut self) {
        let Some(item) = self.runtime.core.items.current_item_id() else {
            return;
        };
        if let Some(preparing) = self.sync.preparing()
            && self
                .runtime
                .core
                .items
                .track_grid(item)
                .is_none_or(|grid| grid.id != preparing.stamp.target)
        {
            let _ = self
                .sync
                .adopt_free(kithara_sync::SyncExecutionReceipt::Rejected {
                    stamp: preparing.stamp,
                    reason: kithara_sync::SyncExecutionReject::Superseded,
                });
            return;
        }
        let Some(receipt) = self.runtime.core.items.free_adoption_receipt(item) else {
            return;
        };
        if receipt.item() == item {
            let receipt = match receipt {
                crate::worker::FreeAdoptionReceipt::Installed(value) => {
                    kithara_sync::SyncExecutionReceipt::Installed {
                        stamp: value.stamp,
                        alignment: value.alignment,
                    }
                }
                crate::worker::FreeAdoptionReceipt::Rejected(value) => {
                    let reason = match value.reason {
                        crate::worker::FreeAdoptionRejectReason::Superseded => {
                            kithara_sync::SyncExecutionReject::Superseded
                        }
                        crate::worker::FreeAdoptionRejectReason::Geometry => {
                            kithara_sync::SyncExecutionReject::Geometry
                        }
                        crate::worker::FreeAdoptionRejectReason::Closed
                        | crate::worker::FreeAdoptionRejectReason::DecodeEpoch
                        | crate::worker::FreeAdoptionRejectReason::SeekEpoch => {
                            kithara_sync::SyncExecutionReject::Unavailable
                        }
                    };
                    kithara_sync::SyncExecutionReceipt::Rejected {
                        stamp: value.stamp,
                        reason,
                    }
                }
            };
            let _ = self.sync.adopt_free(receipt);
        }
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

pub(super) fn source_cue_beat(grid: &BeatGridSnapshot, cue: AssetFrame) -> Option<Beat> {
    let point = MapPoint::new(grid.stamp(), MapPosition::Asset(cue));
    let BeatGridQuery::Resolved(beat) = grid.beat_at_or_next(point) else {
        return None;
    };
    Some(*beat.value().value())
}

/// One output-frame coordinate on the grid's own axis, rounded to a whole
/// frame because every position sync compares is a whole frame.
pub(super) fn native_frame(output_frame: u64, axis: MapAxis, output_rate: NonZeroU32) -> u64 {
    axis.native_frame(output_frame, output_rate)
        .round()
        .to_u64()
        .unwrap_or_default()
}

/// The media position of output-rate `source` frames.
pub(super) fn source_duration(
    source: u64,
    output_rate: NonZeroU32,
) -> kithara_platform::time::Duration {
    let sample_rate = u64::from(output_rate.get());
    kithara_platform::time::Duration::from_secs(source / sample_rate)
        + kithara_platform::time::Duration::from_nanos(
            source % sample_rate * 1_000_000_000 / sample_rate,
        )
}

fn prepared_is_presented(
    frontier: PresentationFrontier,
    activation: kithara_warp::SessionFrame,
    warp_map: kithara_warp::WarpMapRevision,
) -> bool {
    frontier.output() >= activation && frontier.warp_map() == Some(warp_map)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_warp::{PresentationFrontier, SessionFrame, WarpMapRevision};

    use super::prepared_is_presented;

    #[kithara::test]
    fn buffered_pcm_from_the_previous_map_cannot_acknowledge_a_prepared_map() {
        let activation = SessionFrame::new(24_000);
        let revision = WarpMapRevision::first();
        let old_pcm = PresentationFrontier::builder()
            .source(24_000)
            .output(activation)
            .build();
        let applied_pcm = PresentationFrontier::builder()
            .source(24_000)
            .output(activation)
            .warp_map(revision)
            .build();

        assert!(!prepared_is_presented(old_pcm, activation, revision));
        assert!(prepared_is_presented(applied_pcm, activation, revision));
    }
}

/// Region planning for the deck: the plan counts the output frames the
/// decoder emits, so it is owned separately from grid reconciliation.
impl<S> PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    /// Commits the owner's session anchor.
    ///
    /// Crossing an axis boundary withdraws the launch, plan activation and
    /// Free handoff prepared on the previous axis. Once the successor axis is
    /// live, the current track is reconciled onto it with the same launch
    /// disposition a grid publication derives.
    pub(crate) fn commit_session_anchor(
        &mut self,
        anchor: kithara_warp::SessionAnchor,
    ) -> Result<(), SyncError> {
        let before = self.sync.snapshot().state();
        let item = self.runtime.core.items.current_item_id();
        let withdraws = self.sync.crosses_axis_boundary(anchor);
        let member = item
            .and_then(|item| self.runtime.core.items.track_grid(item))
            .map(|grid| grid.id);
        let prepared = member.and_then(|member| self.sync.prepared().get(member));
        let withdraws_prepared = withdraws && !self.sync.prepared().is_empty();
        if withdraws_prepared
            && let (Some(slot), Some(item)) = (self.runtime.slot(), item)
            && !self.runtime.core.engine.cancel_prepared_launches(
                slot,
                item,
                self.runtime.phase_kind() == crate::player::state::phase::PlayerPhaseKind::Playing,
            )
        {
            return Err(SyncError::SlotChannelFull);
        }
        let retargets = member.is_some_and(|member| self.sync.retargets_tempo(member, anchor))
            && self.runtime.phase_kind() == crate::player::state::phase::PlayerPhaseKind::Playing;
        let withdraws_preparing = withdraws && self.sync.preparing().is_some();
        let expected = prepared.map(|prepared| prepared.activation);
        self.sync.publish_session_anchor(anchor)?;
        let Some(item) = item else {
            return Ok(());
        };
        if withdraws_preparing {
            self.runtime.core.items.cancel_outgoing_free_adoption(item);
        }
        if withdraws_prepared {
            self.replan_track(item);
        }
        let reanchored = match member {
            Some(member) => self.sync.reanchored_prepared(member, anchor)?,
            None => None,
        };
        if let (Some(expected), Some(successor), Some(slot)) =
            (expected, reanchored, self.runtime.slot())
        {
            let reanchor = ScheduledSeekReanchor {
                item_id: item,
                position: source_duration(
                    successor.source,
                    self.runtime.core.engine.output_sample_rate(),
                ),
                expected,
                successor: PreparedLaunchIdentity {
                    activation: successor.activation,
                    warp_map: successor.warp_map,
                },
            };
            if self
                .runtime
                .core
                .engine
                .reanchor_scheduled_seek(slot, reanchor, || {
                    self.install_prepared_plan(item, &successor);
                })
            {
                self.sync.adopt_reanchored(successor);
            }
        }
        if retargets {
            return self.retarget_audible(item, anchor.frame());
        }
        if self.sync.mode() != kithara_warp::SyncMode::HostSync
            || !matches!(before, BeatGridState::Unavailable(_))
            || self.sync.snapshot().state() != BeatGridState::Live
        {
            return Ok(());
        }
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return Ok(());
        };
        self.runtime.retire_presented_source_cue(item);
        let (prepared_launch, source_cue) = self.await_prepared_launch(item, &grid.snapshot);
        self.reconcile_item_grid(
            item,
            ReconcileCause::TransportChanged,
            None,
            prepared_launch,
            source_cue,
        )
        .map(|_| ())
        .map_err(|rejected| {
            let (error, _) = rejected.into();
            error
        })
    }

    /// Replaces the audible mapping after a same-axis tempo commit.
    ///
    /// The decoder continues through the source it presents one response
    /// span from now, so the queued PCM rendered at the previous tempo is
    /// replaced instead of drained.
    /// The replacement never starts before `commit`, the frame the new tempo
    /// takes over the session.
    fn retarget_audible(
        &mut self,
        item: TrackId,
        commit: kithara_warp::SessionFrame,
    ) -> Result<(), SyncError> {
        let lead = match self.runtime.core.engine.response_frames() {
            Ok(Some(lead)) => lead,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(%error, %item, "tempo retarget has no response geometry");
                return Err(SyncError::OwnerUnavailable);
            }
        };
        let (Some(grid), Some(snapshot)) = (
            self.runtime.core.items.track_grid(item),
            self.runtime.playback_snapshot(),
        ) else {
            return Ok(());
        };
        let output_rate = self.runtime.core.engine.output_sample_rate();
        let axis = grid.snapshot.axis();
        let observed = self.runtime.presentation_frontier();
        let until_commit = i64::from(commit)
            .saturating_sub(i64::from(observed.output()))
            .to_usize()
            .unwrap_or(0);
        let advance = (lead.get().max(until_commit).to_f64().unwrap_or(f64::MAX)
            * f64::from(snapshot.rate.max(0.0)))
        .ceil()
        .to_u64()
        .unwrap_or(u64::MAX);
        let source = AlignmentSource::Audible {
            presentation: observed.on_axis(axis, output_rate),
            preparation_source: native_frame(
                observed.source().saturating_add(advance),
                axis,
                output_rate,
            ),
            playback_rate: kithara_warp::RateTarget::default().with_speed(snapshot.rate),
        };
        self.reconcile_item_grid(
            item,
            ReconcileCause::TempoRetargeted,
            Some(source),
            false,
            None,
        )
        .map(|_| ())
        .map_err(|rejected| {
            let (error, _) = rejected.into();
            error
        })
    }

    /// Whether the track's selected cue resolves on `grid` and so keeps
    /// waiting for a synchronized launch.
    fn await_prepared_launch(
        &self,
        item: TrackId,
        grid: &BeatGridSnapshot,
    ) -> (bool, Option<Beat>) {
        let source_cue = self
            .runtime
            .core
            .items
            .initial_source_cue(item)
            .and_then(|cue| source_cue_beat(grid, cue));
        let cue_is_owned = self
            .runtime
            .core
            .items
            .await_initial_source_cue_if(item, source_cue.is_some());
        let prepared_launch = self.sync.mode() == kithara_warp::SyncMode::HostSync
            && (self.runtime.presentation_frontier_for(item, None).is_none() || cue_is_owned);
        (
            prepared_launch,
            prepared_launch.then_some(source_cue).flatten(),
        )
    }

    /// Installs the track's own grid as its plan, unprojected.
    ///
    /// A track that holds no prepared map follows nothing yet, so it is heard
    /// as recorded. The projection arrives only with the map that aligns it.
    fn replan_track(&self, item: TrackId) {
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return;
        };
        self.runtime
            .core
            .items
            .set_track_plan(item, Some(Arc::new(WarpPlan::new(grid.snapshot.clone()))));
    }
}
