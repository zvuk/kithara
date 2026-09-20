use std::fmt;

use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::maybe_send::{MaybeSend, MaybeSync};
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, ReconcileCause, SegmentSet,
    SessionAnchor, SessionFrame, SyncAdmission, SyncApplied, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncIntent, SyncMode, SyncOperation, SyncRejected, SyncStatusSnapshot,
    TransportOperation,
};
#[cfg(not(target_arch = "wasm32"))]
use {
    kithara_platform::sync::Arc,
    kithara_test_macros as kithara,
    kithara_warp::{TransportRevision, WarpMap, WarpPlan},
    num_traits::ToPrimitive,
};

use super::{PlaybackView, PlayerImpl, PlayerRuntime};
use crate::{PlayError, SessionBinding, api::TrackId, bridge::PlayerCmd};

#[cfg(not(target_arch = "wasm32"))]
#[path = "protocol/native.rs"]
mod target;
#[cfg(target_arch = "wasm32")]
#[path = "protocol/wasm.rs"]
mod target;

pub use target::PlayerMember;
pub(crate) use target::PlayerSync;

impl fmt::Debug for PlayerMember {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerMember")
            .field("grid_id", &self.id())
            .finish_non_exhaustive()
    }
}

/// Canonical object-safe protocol implemented by a standalone player and its
/// orchestration decorators.
///
/// Queue-specific item, EQ, volume, and event APIs remain on their concrete
/// facade. This contract contains only playback operations shared by every
/// host member plus the synchronization-group protocol.
pub trait Player:
    BeatGrid + SyncGroup<NestedGroup = PlayerMember> + MaybeSend + MaybeSync + 'static
{
    /// Acknowledges the prepared warp map once the deck has rendered up to its
    /// activation frame; `None` when nothing is due yet.
    fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError>;

    /// Stop owned work and detach the player from its playback session.
    fn close(&mut self) -> Result<(), PlayError>;

    /// Reconciles the initial host-synced track and prepares queued launches.
    ///
    /// The owner moves the session axis, so the deadline a waiting track has
    /// to enter by moves with it: the entries are planned on the owner's pass
    /// over its decks, beside the acknowledgement of what the deck plays.
    fn prepare_sync_launches(&mut self, output_now: SessionFrame) -> Result<(), PlayError>;

    /// Records the parent's committed session anchor; a deck under
    /// `HostSync` republishes its session grid on it.
    fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError>;

    /// Read the desired host-applied deck level.
    fn host_level(&self) -> f32;

    /// Pause playback.
    fn pause(&self);

    /// Start or resume playback.
    fn play(&self);

    /// Publishes the asset grid of one queued track on this deck's sync group
    /// and reconciles the track onto the deck.
    fn publish_item_grid(
        &mut self,
        item: TrackId,
        segments: SegmentSet,
        state: BeatGridState,
    ) -> Result<SyncAdmission, SyncError>;

    /// Read one coherent playback view.
    fn playback_view(&self) -> PlaybackView;

    /// Seek within the current item.
    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError>;

    /// Validates and commits a deck seek through the canonical Host owner.
    #[cfg(not(target_arch = "wasm32"))]
    fn seek_from_host(
        &mut self,
        seconds: f64,
        transport: TransportRevision,
    ) -> Result<SeekOutcome, PlayError>;

    /// Commit the host-applied deck level after a validated graph batch.
    fn set_host_level(&self, level: f32);

    /// Advance control-plane and audio-backend work.
    fn tick(&mut self) -> Result<(), PlayError>;
}

/// Produces a cloneable command capability without sharing player identity or
/// synchronization topology.
pub trait PlayerControlSource: Player {
    /// Concrete command capability retained by typed host-owned handles.
    type Control: Clone + MaybeSend + MaybeSync + 'static;

    /// Typed pool schema shared with the canonical playback session.
    type Schema;

    /// Attaches the resident Player to its canonical session exactly once.
    fn attach_session(&mut self, binding: SessionBinding<Self::Schema>) -> Result<(), PlayError>;

    /// Prepare the attached graph and slot before exposing musical controls.
    fn prepare_control(control: &Self::Control) -> Result<(), PlayError>;

    /// Closes the resident player through a previously issued capability.
    fn close_control(control: &Self::Control) -> Result<(), PlayError>;

    /// Creates a command capability for this player.
    fn control(&self) -> Self::Control;

    /// Transfers only the sendable Host-owned part of a wasm player.
    #[cfg(target_arch = "wasm32")]
    #[doc(hidden)]
    fn take_host_member(&mut self) -> Result<PlayerMember, PlayError>;
}

impl<S> BeatGrid for PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    delegate::delegate! {
        to self.sync {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl<S> PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    /// Whether the current track can hand its own mapping to a free
    /// adoption: it must be resident, measured on its own asset axis and
    /// already holding a transition to adopt.
    fn owns_free_adoption(&self) -> bool {
        let Some(item) = self.runtime.core.items.current_item_id() else {
            return false;
        };
        let Some(grid) = self.runtime.core.items.track_grid(item) else {
            return false;
        };
        matches!(grid.snapshot.axis(), kithara_warp::MapAxis::Asset(_))
            && self
                .runtime
                .core
                .items
                .free_adoption_transition(item)
                .is_some()
    }
}

impl<S> SyncGroup for PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    type NestedGroup = PlayerMember;

    fn status(&self) -> SyncStatusSnapshot {
        SyncGroup::status(&self.sync)
    }

    /// Applies one synchronization operation to this deck's group.
    ///
    /// Enabling sync commits the mode even when reconciliation must wait for a
    /// grid, so the selected cue is retained before public play can release
    /// ordinary PCM.
    fn transact(
        &mut self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        let align_now = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::AlignNow,
                ..
            }
        );
        let prepared_launch = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Enable,
                source: kithara_warp::AlignmentSource::Prepared(_),
                ..
            }
        );
        let sync_enable = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Enable,
                ..
            }
        );
        let free = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Free,
                ..
            }
        );
        let operation = self.with_free_source(operation)?;
        let sync_disable = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Disable,
                ..
            }
        );
        if free && !self.owns_free_adoption() {
            return Err(SyncRejected::new(SyncError::OwnerUnavailable, operation));
        }
        let alignment_source = match &operation {
            SyncOperation::Sync { source, .. } => Some(*source),
            SyncOperation::Transport {
                operation: TransportOperation::Seek { source_frame },
                ..
            } => Some(kithara_warp::AlignmentSource::Prepared(
                kithara_warp::PresentationFrontier::builder()
                    .source(*source_frame)
                    .output(self.runtime.presentation_frontier().output())
                    .build(),
            )),
            _ => None,
        };
        let reconcile_transport = alignment_source.is_some()
            && matches!(&operation, SyncOperation::Transport { .. })
            && self.sync.mode() == SyncMode::HostSync;
        let preparing_before = self
            .sync
            .preparing()
            .map(|preparing| (preparing.stamp.operation, preparing.stamp.successor));
        if sync_disable
            && let (Some(slot), Some(item)) = (
                self.runtime.slot(),
                self.runtime.core.items.current_item_id(),
            )
            && !self.runtime.core.engine.cancel_prepared_launches(
                slot,
                item,
                self.runtime.phase_kind() == crate::player::state::phase::PlayerPhaseKind::Playing,
            )
        {
            return Err(SyncRejected::new(SyncError::SlotChannelFull, operation));
        }
        let now = self.runtime.presentation_frontier().output();
        let result = if let (Some(item), Some((operation_id, warp_map))) =
            (self.runtime.core.items.current_item_id(), preparing_before)
        {
            self.runtime
                .core
                .items
                .transition_free_adoption(item, operation_id, warp_map, || {
                    let result = self.sync.transact_at(operation, now);
                    let revoke = result.is_ok()
                        && self.sync.preparing().is_none_or(|current| {
                            (current.stamp.operation, current.stamp.successor)
                                != (operation_id, warp_map)
                        });
                    (result, revoke)
                })
        } else {
            self.sync.transact_at(operation, now)
        };
        let admission = result?;
        if free && matches!(admission, SyncAdmission::Preparing { .. }) {
            self.prepare_free_handoff();
        }
        if sync_enable
            && self.sync.mode() == SyncMode::HostSync
            && let Some(item) = self.runtime.core.items.current_item_id()
        {
            self.runtime.retire_presented_source_cue(item);
            self.runtime.core.items.await_initial_source_cue(item);
        }
        if sync_disable
            && self.sync.mode() != SyncMode::HostSync
            && let Some(item) = self.runtime.core.items.current_item_id()
            && self
                .runtime
                .core
                .items
                .consume_awaiting_initial_source_cue(item)
            && self.runtime.phase_kind() == crate::player::state::phase::PlayerPhaseKind::Playing
        {
            let _ = self.runtime.send_to_slot(PlayerCmd::SetPaused {
                paused: false,
                item_id: Some(item),
            });
        }
        let reconcile_cause = if align_now {
            ReconcileCause::AlignmentRequested
        } else {
            ReconcileCause::TransportChanged
        };
        if (align_now
            || reconcile_transport
            || matches!(admission, SyncAdmission::StateChanged { .. }))
            && let Some(reconciled) =
                self.reconcile_current_grid(reconcile_cause, alignment_source, prepared_launch)?
        {
            return Ok(reconciled);
        }
        Ok(admission)
    }

    delegate::delegate! {
        to self.sync {
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}

impl<S> Player for PlayerImpl<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError> {
        Self::acknowledge_prepared(self)
    }

    fn close(&mut self) -> Result<(), PlayError> {
        self.make_control().close()
    }

    fn prepare_sync_launches(&mut self, output_now: SessionFrame) -> Result<(), PlayError> {
        Self::prepare_sync_launches(self, output_now)
    }

    fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError> {
        Self::commit_session_anchor(self, anchor)
    }

    fn host_level(&self) -> f32 {
        self.runtime.core.engine.master_volume()
    }

    fn pause(&self) {
        let _ = self.runtime.with_open(PlayerRuntime::pause);
    }

    fn play(&self) {
        let _ = self.runtime.with_open(PlayerRuntime::play);
    }

    fn publish_item_grid(
        &mut self,
        item: TrackId,
        segments: SegmentSet,
        state: BeatGridState,
    ) -> Result<SyncAdmission, SyncError> {
        Self::publish_item_grid(self, item, segments, state)
    }

    fn playback_view(&self) -> PlaybackView {
        if self.runtime.is_closed() {
            return PlaybackView::default();
        }
        self.runtime
            .playback_snapshot()
            .map(PlaybackView::from)
            .unwrap_or_default()
    }

    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.runtime
            .with_open_result(|runtime| runtime.seek_seconds(seconds))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn seek_from_host(
        &mut self,
        seconds: f64,
        transport: TransportRevision,
    ) -> Result<SeekOutcome, PlayError> {
        if !seconds.is_finite() {
            return Err(PlayError::InvalidHostSeekPosition { seconds });
        }
        let seconds = seconds.max(0.0);
        if self.sync.mode() != SyncMode::HostSync {
            return self.seek_seconds(seconds);
        }
        let preparing_before = self
            .sync
            .preparing()
            .map(|preparing| (preparing.stamp.operation, preparing.stamp.successor));
        let prepared = self.prepare_host_seek(seconds)?;
        let source = kithara_warp::AlignmentSource::Prepared(
            kithara_warp::PresentationFrontier::builder()
                .source(prepared.source_frame)
                .output(prepared.activation_floor)
                .build(),
        );
        let reconcile =
            crate::sync::host_seek::prepare(&self.sync, prepared.grid_stamp, source, transport)
                .map_err(PlayError::from)?;
        let output_rate = self.runtime.core.engine.output_sample_rate();
        let destination = target::duration_for_source(reconcile.prepared.source, output_rate.get());
        let plan = Arc::new(
            WarpPlan::new(reconcile.prepared.projection.clone()).with_activation(
                WarpMap::identity(reconcile.prepared.warp_map).reanchor(
                    reconcile.prepared.source,
                    reconcile.prepared.activation,
                    reconcile.prepared.activation_beat,
                ),
            ),
        );
        let disposition = target::host_seek_disposition(
            reconcile.prepared.activation,
            reconcile.prepared.warp_map,
            self.runtime
                .playback_snapshot()
                .is_some_and(|snapshot| snapshot.is_playing()),
        );
        let items = &self.runtime.core.items;
        let sync = &mut self.sync;
        let transition = items.free_adoption_transition(prepared.item);
        let revoke = std::cell::Cell::new(false);
        let commit = || {
            self.runtime.core.engine.commit_validated_track_seek(
                prepared.slot,
                prepared.item,
                destination,
                disposition,
                || {
                    items.commit_current_track_plan(
                        prepared.item,
                        prepared.grid_stamp,
                        plan,
                        || {
                            let result = crate::sync::host_seek::commit(sync, reconcile.clone())
                                .map_err(PlayError::from);
                            revoke.set(preparing_before.is_some_and(|identity| {
                                result.is_ok()
                                    && sync.preparing().is_none_or(|current| {
                                        (current.stamp.operation, current.stamp.successor)
                                            != identity
                                    })
                            }));
                            result
                        },
                    )
                },
            )
        };
        let result = if let (Some(transition), Some((operation, warp_map))) =
            (transition, preparing_before)
        {
            transition.transition(operation, warp_map, || {
                let result = commit();
                let accepted = result.is_ok();
                (result, accepted && revoke.get())
            })
        } else {
            commit()
        };
        let _ = result?;
        if disposition.is_prepared_launch() {
            self.arm_prepared_launch_while_playing(prepared.slot, prepared.item);
        }
        kithara::probe_event!(
            warp_plan_published,
            warp_map_revision = u64::from(reconcile.prepared.warp_map),
            presentation_source = prepared.source_frame,
            preparation_source = prepared.source_frame,
            activation_source = reconcile.prepared.source,
            activation_output = i64::from(reconcile.prepared.activation)
        );
        Ok(target::seek_outcome(
            kithara_platform::time::Duration::from_secs_f64(seconds),
            destination,
            self.runtime.duration_seconds(),
        ))
    }

    fn set_host_level(&self, level: f32) {
        if !self.runtime.is_closed() {
            self.runtime.core.engine.commit_desired_master_volume(level);
        }
    }

    fn tick(&mut self) -> Result<(), PlayError> {
        self.runtime.with_open_result(PlayerRuntime::tick)?;
        Player::acknowledge_prepared(self)
            .map(|_| ())
            .map_err(PlayError::from)
    }
}

impl<S> PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    fn free_handoff_source(&self) -> Option<kithara_warp::AlignmentSource> {
        let item = self.runtime.core.items.current_item_id()?;
        let observed = self.runtime.presentation_frontier_for(item, None)?;
        let axis = self.runtime.core.items.track_grid(item)?.snapshot.axis();
        let output_rate = self.runtime.core.engine.output_sample_rate();
        let presentation = observed.on_axis(axis, output_rate);
        let manual_rate = self.runtime.core.warp.stretch().rate_target();
        Some(kithara_warp::AlignmentSource::Audible {
            presentation,
            preparation_source: presentation.source(),
            playback_rate: manual_rate,
        })
    }

    fn with_free_source(
        &self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncOperation<PlayerMember>, SyncRejected<PlayerMember>> {
        if !matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Free,
                ..
            }
        ) {
            return Ok(operation);
        }
        let Some(source) = self.free_handoff_source() else {
            return Err(SyncRejected::new(SyncError::OwnerUnavailable, operation));
        };
        let SyncOperation::Sync {
            target,
            load,
            transport,
            activation,
            intent,
            ..
        } = operation
        else {
            unreachable!("Free is only a sync operation");
        };
        Ok(SyncOperation::Sync {
            target,
            load,
            transport,
            source,
            activation,
            intent,
        })
    }
}

impl<S> PlayerImpl<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn prepare_host_seek(&self, seconds: f64) -> Result<target::PreparedHostSeek, PlayError> {
        if !seconds.is_finite() {
            return Err(PlayError::InvalidHostSeekPosition { seconds });
        }
        let seconds = seconds.max(0.0);
        let item = self
            .runtime
            .core
            .items
            .current_item_id()
            .ok_or(PlayError::NoCurrentItem)?;
        let grid = self
            .runtime
            .core
            .items
            .track_grid(item)
            .ok_or(PlayError::MissingTrackGrid { item })?;
        let grid_stamp = grid.snapshot.stamp();
        let slot = self.runtime.slot().ok_or(PlayError::NoActiveSlot)?;
        self.runtime.core.engine.validate_track_seek(slot, item)?;
        let axis = grid.snapshot.axis();
        let source_frame = (seconds * f64::from(axis.sample_rate().get())).round();
        let source_frame = source_frame
            .to_u64()
            .ok_or(PlayError::InvalidHostSeekPosition { seconds })?;
        let frontier = self.runtime.presentation_frontier();
        let lead = i64::try_from(self.runtime.core.response_budget_frames.get())
            .map_err(|_| PlayError::InvalidHostSeekPosition { seconds })?;
        let activation_floor = i64::from(frontier.output())
            .checked_add(lead)
            .map(SessionFrame::new)
            .ok_or(PlayError::InvalidHostSeekPosition { seconds })?;
        Ok(target::PreparedHostSeek {
            activation_floor,
            item,
            grid_stamp,
            slot,
            source_frame,
        })
    }
}

impl<S> PlayerControlSource for PlayerImpl<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Control = crate::player::PlayerControl<S>;
    type Schema = S;

    fn attach_session(&mut self, binding: SessionBinding<S>) -> Result<(), PlayError> {
        self.runtime.attach_session(binding)
    }

    fn prepare_control(control: &Self::Control) -> Result<(), PlayError> {
        control.prepare()
    }

    fn close_control(control: &Self::Control) -> Result<(), PlayError> {
        control.close()
    }

    fn control(&self) -> Self::Control {
        self.make_control()
    }

    #[cfg(target_arch = "wasm32")]
    fn take_host_member(&mut self) -> Result<PlayerMember, PlayError> {
        let sync = self.sync.take().ok_or_else(|| {
            PlayError::Internal("player synchronization ownership was already transferred".into())
        })?;
        Ok(PlayerMember::new(
            sync,
            self.runtime.core.engine.master_volume(),
        ))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use kithara_warp::SyncMemberKind;

    use super::*;
    use crate::{
        GroupState, PlayWorker, PlayWorkerConfig, mock,
        player::PlayerConfig,
        test_pools::{TestPools, pools},
    };

    fn player() -> PlayerImpl<TestPools> {
        PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
                .session(mock::session())
                .build(),
        )
    }

    #[kithara::test]
    fn host_seek_rejects_nonfinite_without_advancing_sync() {
        let player = player();
        let before = player.sync.generations();

        let Err(error) = player.prepare_host_seek(f64::NAN) else {
            panic!("NaN is invalid");
        };

        assert!(matches!(error, PlayError::InvalidHostSeekPosition { .. }));
        assert_eq!(player.sync.generations(), before);
    }

    #[kithara::test]
    fn host_seek_requires_a_current_item_before_mutation() {
        let player = player();
        let before = player.sync.generations();

        let Err(error) = player.prepare_host_seek(1.0) else {
            panic!("empty player has no item");
        };

        assert!(matches!(error, PlayError::NoCurrentItem));
        assert_eq!(player.sync.generations(), before);
    }

    #[kithara::test]
    fn host_seek_receipt_keeps_the_request_and_reports_the_quantized_destination() {
        let destination = target::duration_for_source(24_000, 48_000);
        let request = Duration::from_millis(510);

        assert_eq!(destination, Duration::from_millis(500));
        assert!(matches!(
            target::seek_outcome(request, destination, Some(1.0)),
            SeekOutcome::Landed { target, landed_at } if target == request && landed_at == destination
        ));
    }

    #[kithara::test]
    #[case::off(SyncMode::Off)]
    #[case::local(SyncMode::LocalSync)]
    fn host_seek_delegates_directly_without_a_grid(#[case] mode: SyncMode) {
        let mut player = player();
        player.sync = GroupState::new(player.sync.snapshot(), SyncMemberKind::Grid, mode);

        let outcome = player
            .seek_from_host(1.0, TransportRevision::first())
            .expect("non-HostSync seek delegates to the resident player");

        assert!(matches!(outcome, SeekOutcome::Landed { target, landed_at }
            if target == Duration::from_secs(1) && landed_at == target));
    }

    #[kithara::test]
    fn host_seek_launches_a_deck_that_is_not_yet_audible_at_the_cue() {
        let activation = SessionFrame::new(96_000);
        let warp_map = kithara_warp::WarpMapRevision::first();

        assert!(matches!(
            target::host_seek_disposition(activation, warp_map, false),
            crate::bridge::ScheduledSeekDisposition::PreparedLaunch(identity)
                if identity == crate::bridge::PreparedLaunchIdentity { activation, warp_map }
        ));
    }

    #[kithara::test]
    fn host_seek_keeps_the_audible_stream_until_the_activation() {
        let activation = SessionFrame::new(96_000);

        assert!(matches!(
            target::host_seek_disposition(activation, kithara_warp::WarpMapRevision::first(), true),
            crate::bridge::ScheduledSeekDisposition::SeekOnly { activation: scheduled }
                if scheduled == activation
        ));
    }
}
