use std::fmt;

use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::maybe_send::{MaybeSend, MaybeSync};
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, ReconcileCause, SegmentSet,
    SessionAnchor, SyncAdmission, SyncApplied, SyncError, SyncGroup, SyncGroupSnapshot, SyncIntent,
    SyncMode, SyncOperation, SyncRejected, SyncStatusSnapshot, TransportOperation,
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

    /// Commit the host-applied deck level after a validated graph batch.
    fn set_host_level(&self, level: f32);

    /// Advance control-plane and audio-backend work.
    fn tick(&self) -> Result<(), PlayError>;
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

impl<S> SyncGroup for PlayerImpl<S>
where
    S: Send + Sync + 'static,
{
    type NestedGroup = PlayerMember;

    fn status(&self) -> SyncStatusSnapshot {
        SyncGroup::status(&self.sync)
    }

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
        let sync_release = matches!(
            &operation,
            SyncOperation::Sync {
                intent: SyncIntent::Disable | SyncIntent::Free,
                ..
            }
        );
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
        if sync_release
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
        let (admission, projection) = self.sync.transact_at(operation, now)?;
        // The mode transition has committed even when reconciliation must wait
        // for a grid, so retain the selected cue before public play can release
        // ordinary PCM.
        if sync_enable
            && self.sync.mode() == SyncMode::HostSync
            && let Some(item) = self.runtime.core.items.current_item_id()
        {
            self.runtime.core.items.await_initial_source_cue(item);
        }
        if sync_release
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
            if let Some(projection) = projection {
                self.runtime.core.engine.publish_deck_grid(projection);
            }
            return Ok(reconciled);
        }
        if let Some(projection) = projection {
            self.runtime.core.engine.publish_deck_grid(projection);
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

    fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError> {
        self.sync.publish_session_anchor(anchor)?;
        Ok(())
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

    fn set_host_level(&self, level: f32) {
        if !self.runtime.is_closed() {
            self.runtime.core.engine.commit_desired_master_volume(level);
        }
    }

    fn tick(&self) -> Result<(), PlayError> {
        self.runtime.with_open_result(PlayerRuntime::tick)
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
