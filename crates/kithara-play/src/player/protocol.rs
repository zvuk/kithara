use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::maybe_send::{MaybeSend, MaybeSync};
use kithara_sync::SyncAttachment;
use kithara_warp::{BeatGrid, BeatGridId};

use super::{PlaybackView, PlayerImpl, PlayerRuntime, ResidentLoadObservation};
use crate::{PlayError, SessionBinding};

/// Canonical object-safe protocol implemented by a standalone player and its
/// orchestration decorators.
///
/// Queue-specific item, EQ, volume, and event APIs remain on their concrete
/// facade. This contract contains only playback operations shared by every
/// host member; synchronization attaches through
/// [`PlayerControlSource::attach_session`].
pub trait Player: MaybeSend + MaybeSync + 'static {
    /// Stop owned work and detach the player from its playback session.
    fn close(&mut self) -> Result<(), PlayError>;

    /// Read the desired host-applied deck level.
    fn host_level(&self) -> f32;

    /// Pause playback.
    fn pause(&self);

    /// Start or resume playback.
    fn play(&self);

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

    /// Grid identity of the deck group that owns this player's track.
    fn sync_group_grid_id(&self) -> BeatGridId;

    /// Grid identity of the direct track whose prepared lane this player owns.
    fn sync_track_grid_id(&self) -> BeatGridId;

    /// One committed resident load and its matching live render observation.
    /// The Host validates these facts under its own gate before admission.
    ///
    /// # Errors
    /// Returns an error when the player command capability is closed.
    fn resident_sync_observation(
        control: &Self::Control,
    ) -> Result<Option<ResidentLoadObservation>, PlayError>;

    /// Attaches the resident Player to its canonical session exactly once and
    /// hands that owner the player's synchronization attachment: the group
    /// identity, the track geometry and the executor of its staged lanes. The
    /// session is the only owner the attachment ever exists for.
    fn attach_session(
        &mut self,
        binding: SessionBinding<Self::Schema>,
    ) -> Result<SyncAttachment, PlayError>;

    /// Closes the resident player through a previously issued capability.
    fn close_control(control: &Self::Control) -> Result<(), PlayError>;

    /// Creates a command capability for this player.
    fn control(&self) -> Self::Control;

    /// Prepare the attached graph and slot before exposing musical controls.
    fn prepare_control(control: &Self::Control) -> Result<(), PlayError>;
}

impl<S> Player for PlayerImpl<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn close(&mut self) -> Result<(), PlayError> {
        self.make_control().close()
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

    fn sync_group_grid_id(&self) -> BeatGridId {
        self.grid_id
    }

    fn sync_track_grid_id(&self) -> BeatGridId {
        self.runtime.core.track_grid.id()
    }

    fn resident_sync_observation(
        control: &Self::Control,
    ) -> Result<Option<ResidentLoadObservation>, PlayError> {
        control.resident_sync_observation()
    }

    fn attach_session(&mut self, binding: SessionBinding<S>) -> Result<SyncAttachment, PlayError> {
        self.runtime.attach_session(binding)?;
        Ok(SyncAttachment::new(
            self.grid_id,
            self.sample_rate,
            Box::new(self.runtime.core.track_grid.clone()),
            self.runtime.core.staging.execution(),
        ))
    }

    fn close_control(control: &Self::Control) -> Result<(), PlayError> {
        control.close()
    }

    fn control(&self) -> Self::Control {
        self.make_control()
    }

    fn prepare_control(control: &Self::Control) -> Result<(), PlayError> {
        control.prepare()
    }
}
