use kithara_bufpool::HasPool;
#[cfg(not(target_arch = "wasm32"))]
use kithara_play::TransportRevision;
use kithara_play::{
    BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridState, PlayError, SeekOutcome, SegmentSet,
    SessionAnchor, SessionBinding, SyncAdmission, SyncApplied, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncOperation, SyncRejected, SyncStatusSnapshot,
    player::{PlaybackView, Player, PlayerControlSource, PlayerMember},
};
use kithara_warp::SessionFrame;

use super::Queue;
use crate::TrackId;

impl<S> BeatGrid for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    delegate::delegate! {
        to self.player {
            fn id(&self) -> BeatGridId;
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl<S> SyncGroup for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type NestedGroup = PlayerMember;

    fn status(&self) -> SyncStatusSnapshot {
        SyncGroup::status(&self.player)
    }

    delegate::delegate! {
        to self.player {
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;

            fn transact(
                &mut self,
                operation: SyncOperation<PlayerMember>,
            ) -> Result<SyncAdmission, SyncRejected<PlayerMember>>;

            fn acknowledge(
                &mut self,
                applied: SyncApplied,
            ) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}

impl<S> Player for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    delegate::delegate! {
        to self.control {
            fn play(&self);
            fn pause(&self);
            fn playback_view(&self) -> PlaybackView;
            fn close(&mut self) -> Result<(), PlayError>;
        }
        to self {
            #[call(seek_player)]
            fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError>;
        }
        to self.player {
            #[cfg(not(target_arch = "wasm32"))]
            fn seek_from_host(
                &mut self,
                seconds: f64,
                transport: TransportRevision,
            ) -> Result<SeekOutcome, PlayError>;
            fn set_host_level(&self, level: f32);
            fn host_level(&self) -> f32;
            fn commit_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError>;
            fn publish_item_grid(
                &mut self,
                item: TrackId,
                segments: SegmentSet,
                state: BeatGridState,
            ) -> Result<SyncAdmission, SyncError>;
            fn acknowledge_prepared(&mut self) -> Result<Option<SyncStatusSnapshot>, SyncError>;
            fn prepare_sync_launches(
                &mut self,
                output_now: SessionFrame,
            ) -> Result<(), PlayError>;
        }
    }

    fn tick(&mut self) -> Result<(), PlayError> {
        let _admission = self.control.lock_admission();
        self.control.ensure_open()?;
        self.player.tick()?;
        self.control.observe_player_tick();
        Ok(())
    }
}

impl<S> PlayerControlSource for Queue<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    type Control = super::QueueControl<S>;
    type Schema = S;

    fn prepare_control(control: &Self::Control) -> Result<(), PlayError> {
        control.with_open_result(|queue| queue.player.prepare())
    }

    fn close_control(control: &Self::Control) -> Result<(), PlayError> {
        control.close()
    }

    fn control(&self) -> Self::Control {
        self.control.clone()
    }

    delegate::delegate! {
        to self.player {
            fn attach_session(&mut self, binding: SessionBinding<S>) -> Result<(), PlayError>;
            #[cfg(target_arch = "wasm32")]
            fn take_host_member(&mut self) -> Result<PlayerMember, PlayError>;
        }
    }
}
