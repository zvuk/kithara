use std::marker::PhantomData;

use kithara_audio::SeekOutcome;
use kithara_platform::{
    maybe_send::BoxFuture,
    sync::{Arc, Weak},
};

use super::{
    component::PlayerComponentBox,
    core::{MemberId, MultiPlayerCore, RoutedMember},
    group::seek_core,
};
use crate::{
    api::{Player, SessionBeat, SessionSeek, StartAt, TrackBinding},
    error::PlayError,
    player::PlayerImpl,
    resource::Resource,
};

/// Routed handle for a component owned by a [`super::MultiPlayer`].
pub struct Member<T> {
    member_id: MemberId,
    component: PhantomData<fn() -> T>,
    owner: Weak<MultiPlayerCore>,
}

impl<T> Member<T> {
    pub(super) fn new(member_id: MemberId, owner: Weak<MultiPlayerCore>) -> Self {
        Self {
            member_id,
            owner,
            component: PhantomData,
        }
    }

    /// Stable direct-member identity in the group that accepted the value.
    #[must_use]
    pub const fn id(&self) -> MemberId {
        self.member_id
    }

    async fn join_player(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> Result<(), PlayError> {
        let route = self.route()?;
        let _transaction = route.owner.begin_control()?;
        let component = route.owner.target(&route.path)?;
        let player = component
            .players
            .first()
            .ok_or(PlayError::PlayerMemberDetached)?;
        if component.players.len() != 1 {
            return Err(PlayError::PlayerMemberNotFound {
                member_id: self.member_id.get(),
            });
        }
        player
            .join_track_at(resource, item_id, binding, target)
            .await
    }

    fn route(&self) -> Result<RoutedMember, PlayError> {
        self.owner
            .upgrade()
            .ok_or(PlayError::PlayerMemberDetached)?
            .resolve_root(self.member_id)
    }

    fn target(&self) -> Result<PlayerComponentBox, PlayError> {
        let route = self.route()?;
        route.owner.target(&route.path)
    }

    fn with_control<R>(
        &self,
        call: impl FnOnce(&dyn Player) -> Result<R, PlayError>,
    ) -> Result<R, PlayError> {
        let route = self.route()?;
        let _transaction = route.owner.begin_control()?;
        let component = route.owner.target(&route.path)?;
        call(component.component.as_ref())
    }

    fn with_seek<R>(
        &self,
        call: impl FnOnce(&dyn Player) -> Result<R, PlayError>,
    ) -> Result<R, PlayError> {
        let route = self.route()?;
        let _transaction = route.owner.begin_seek()?;
        let component = route.owner.target(&route.path)?;
        call(component.component.as_ref())
    }
}

impl Member<PlayerImpl> {
    /// Prepare and join this routed deck at an exact session beat.
    pub async fn join_track_at(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> Result<(), PlayError> {
        self.join_player(resource, item_id, binding, target).await
    }
}

#[cfg(any(test, feature = "probe"))]
impl Member<Arc<PlayerImpl>> {
    pub async fn join_track_at(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> Result<(), PlayError> {
        self.join_player(resource, item_id, binding, target).await
    }
}

impl<T> Player for Member<T>
where
    T: 'static,
{
    fn duration_seconds(&self) -> Option<f64> {
        self.target()
            .ok()
            .and_then(|component| component.component.duration_seconds())
    }

    fn pause(&self) -> Result<(), PlayError> {
        self.with_control(Player::pause)
    }

    fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.with_seek(|player| player.seek_seconds(seconds))
    }

    fn start_at(&self, start: StartAt) -> Result<(), PlayError> {
        self.with_control(|player| player.start_at(start))
    }
}

impl<T> SessionSeek for Member<T>
where
    T: 'static,
{
    fn seek_session(&self, target: SessionBeat) -> BoxFuture<'_, Result<(), PlayError>> {
        Box::pin(async move {
            let route = self.route()?;
            seek_core(&route.owner, target).await
        })
    }
}
