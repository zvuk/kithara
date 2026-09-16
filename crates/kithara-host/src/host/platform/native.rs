use std::{marker::PhantomData, num::NonZeroU32};

use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;
use kithara_play::{
    GroupState, PlayError, SeekOutcome,
    effects::LimiterConfig,
    player::{PlayerControlSource, PlayerMember},
};
use kithara_warp::{BeatGridId, SyncAdmission, SyncOperation, SyncRejected};

use super::super::{Host, HostOwned, PlatformResult};
use crate::session::{HostCmd, HostDispatcher, HostReply, RootView};

type StartedPlatform<S> = (Arc<dyn HostDispatcher<S>>, Platform<S>);

impl<S> PlatformResult<Self> for Platform<S> {
    fn resolve(self) -> Result<Self, PlayError> {
        Ok(self)
    }
}

impl<S> PlatformResult<Self> for StartedPlatform<S> {
    fn resolve(self) -> Result<Self, PlayError> {
        Ok(self)
    }
}

pub(in crate::host) struct Platform<S> {
    marker: PhantomData<fn() -> S>,
}

impl<S> Platform<S> {
    pub(in crate::host) const fn close(_platform: &mut Self, _host_id: BeatGridId) {}

    #[cfg(feature = "offline")]
    pub(in crate::host) const fn offline() -> Self {
        Self::owner()
    }

    pub(in crate::host) const fn owner() -> Self {
        Self {
            marker: PhantomData,
        }
    }

    pub(in crate::host) fn realtime(
        group: GroupState<PlayerMember>,
        view: RootView,
        sample_rate: NonZeroU32,
        output_block_frames: Option<NonZeroU32>,
        limiter: LimiterConfig,
    ) -> StartedPlatform<S>
    where
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let dispatcher = crate::session::native::spawn::<S>(
            group,
            view,
            sample_rate,
            output_block_frames,
            limiter,
        );
        (dispatcher, Self::owner())
    }

    pub(in crate::host) fn transact(
        _platform: &Self,
        dispatcher: &Arc<dyn HostDispatcher<S>>,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        dispatcher.transact(operation)
    }
}

impl<S> Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Seek one Host-owned deck while retaining the Host session clock.
    ///
    /// # Errors
    /// Returns an error when the handle is foreign, the deck is unavailable,
    /// or its canonical owner rejects the request.
    pub fn seek_deck<P>(&self, deck: &HostOwned<P>, seconds: f64) -> Result<SeekOutcome, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        if !seconds.is_finite() {
            return Err(PlayError::InvalidHostSeekPosition { seconds });
        }
        self.validate_removal(deck)?;
        match self.dispatcher.exec_host(HostCmd::SeekDeck {
            deck: deck.id(),
            seconds,
        })? {
            HostReply::Seek(result) => result,
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for deck seek".into(),
            )),
        }
    }

    /// Attaches and transfers one fully configured player or decorator into
    /// this Host, then prepares its graph and initial slot before returning.
    /// Audio-device setup may block; musical playback remains stopped.
    ///
    /// # Errors
    /// Returns an error when binding, attachment, or graph preparation fails.
    pub fn insert<P>(&mut self, mut player: P) -> Result<HostOwned<P>, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        let (grid_id, control) = self.bind_player(&mut player)?;
        self.attach_member(PlayerMember::new(player))?;
        let owned = self.owned::<P>(grid_id, control);
        if let Err(error) = P::prepare_control(owned.control()) {
            self.remove(&owned)?;
            return Err(error);
        }
        Ok(owned)
    }

    /// Closes the lower runtime on the caller thread, then detaches its
    /// canonical member after graph unregistration has completed.
    ///
    /// # Errors
    /// Returns an error when close or canonical detachment fails.
    pub fn remove<P>(&mut self, player: &HostOwned<P>) -> Result<(), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        self.validate_removal(player)?;
        P::close_control(player.control())?;
        self.detach_member(player.id())
    }
}
