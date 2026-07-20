use std::{future::Future, sync::atomic::Ordering};

use kithara_platform::{
    sync::Arc,
    time::{Duration, sleep},
};

use super::super::core::PlayerImpl;
use crate::{
    PlayerId,
    api::{SessionBeat, SlotId, Tempo, TrackBinding},
    bridge::PlayerCmd,
    error::PlayError,
    resource::Resource,
};

/// Session-wide musical mutations executed by the canonical player owner.
///
/// Implementations coordinate the supplied players through their existing
/// queues, slots, and renderers; the trait owns no runtime state or protocol.
pub trait SessionTrackControl {
    /// Adds the first bound track to an idle player at an exact session beat.
    fn join_track_at<'a>(
        &'a self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> impl Future<Output = Result<(), PlayError>> + 'a;

    /// Relocates every participating bound player at one session render boundary.
    fn seek_session<'a>(
        &'a self,
        peers: &'a [&'a Self],
        target: SessionBeat,
    ) -> impl Future<Output = Result<(), PlayError>> + 'a;
}

impl PlayerImpl {
    /// Changes the shared session tempo for this player and its participating peers.
    pub fn set_session_tempo(&self, peers: &[&Self], tempo: Tempo) -> Result<(), PlayError> {
        let participants = self.session_participants(peers)?;
        let playlists: Vec<_> = participants
            .iter()
            .map(|(_, player)| player.core.items.lock_playlist())
            .collect();
        let snapshot = self.core.engine.session_transport()?;
        let preparation = self.core.engine.binding_preparation()?;
        if preparation.revision != snapshot.revision() || preparation.tempo != snapshot.tempo() {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        let shape = preparation.shape;
        for ((_, player), playlist) in participants.iter().zip(&playlists) {
            player.validate_session_tempo(snapshot, tempo, shape, playlist.current_binding())?;
            Self::validate_successor_tempo(playlist, tempo, shape)?;
        }
        let player_ids = participants
            .iter()
            .map(|(player_id, _)| *player_id)
            .collect();
        self.core
            .engine
            .set_session_tempo_checked(tempo, snapshot.revision(), shape, player_ids)
    }

    async fn seek_session(&self, peers: &[&Self], target: SessionBeat) -> Result<(), PlayError> {
        let participants = self.session_participants(peers)?;
        let snapshot = self.core.engine.session_transport()?;
        let preparation = self.core.engine.binding_preparation()?;
        if preparation.revision != snapshot.revision() || preparation.tempo != snapshot.tempo() {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        let revision = snapshot
            .revision()
            .checked_add(1)
            .ok_or_else(|| PlayError::Internal("session transport revision is exhausted".into()))?;
        let shape = preparation.shape;
        let mut bindings = Vec::with_capacity(participants.len());
        let mut playbacks = Vec::with_capacity(participants.len());
        let mut slots = Vec::with_capacity(participants.len());
        let mut preparation_started = false;
        let transaction = async {
            {
                let playlists: Vec<_> = participants
                    .iter()
                    .map(|(_, player)| player.core.items.lock_playlist())
                    .collect();
                for ((_, player), playlist) in participants.iter().zip(&playlists) {
                    player.validate_session_seek(
                        target,
                        snapshot.tempo(),
                        revision,
                        shape,
                        playlist.current_binding(),
                    )?;
                    bindings.push((playlist.current(), playlist.current_binding().cloned()));
                    let slot = player.slot().ok_or(PlayError::NoActiveSlot)?;
                    let playback = player
                        .core
                        .engine
                        .slot_playback(slot)
                        .ok_or(PlayError::SlotNotFound(slot))?;
                    slots.push(slot);
                    playbacks.push(playback);
                }
                preparation_started = true;
                for (((_, player), slot), playback) in
                    participants.iter().zip(&slots).zip(&playbacks)
                {
                    playback.session_seek_prepared.store(0, Ordering::SeqCst);
                    playback.session_seek_failed.store(0, Ordering::SeqCst);
                    player
                        .core
                        .engine
                        .try_send_slot_cmd(
                            *slot,
                            PlayerCmd::PrepareSessionSeek {
                                target,
                                tempo: snapshot.tempo(),
                                revision,
                            },
                        )
                        .map_err(|rejected| rejected.error)?;
                }
            }

            loop {
                if self
                    .core
                    .engine
                    .cancel_token()
                    .is_none_or(|cancel| cancel.is_cancelled())
                {
                    return Err(PlayError::BindingPreparationCancelled);
                }
                let mut ready = true;
                for playback in &playbacks {
                    if playback.session_seek_failed.load(Ordering::SeqCst) == revision {
                        return Err(PlayError::SessionSeekPreparationFailed);
                    }
                    ready &= playback.session_seek_prepared.load(Ordering::SeqCst) == revision;
                }
                if ready {
                    break;
                }
                if self.core.engine.session_transport()?.revision() != snapshot.revision() {
                    return Err(PlayError::BindingPreparationContextChanged);
                }
                sleep(Duration::from_millis(1)).await;
            }

            let playlists: Vec<_> = participants
                .iter()
                .map(|(_, player)| player.core.items.lock_playlist())
                .collect();
            for (((((_, player), playlist), (index, binding)), slot), playback) in participants
                .iter()
                .zip(&playlists)
                .zip(&bindings)
                .zip(&slots)
                .zip(&playbacks)
            {
                if player.slot() != Some(*slot)
                    || playlist.current() != *index
                    || playlist.current_binding() != binding.as_ref()
                {
                    return Err(PlayError::BindingPreparationContextChanged);
                }
                player.validate_session_seek(
                    target,
                    snapshot.tempo(),
                    revision,
                    shape,
                    playlist.current_binding(),
                )?;
                if playback.session_seek_failed.load(Ordering::SeqCst) == revision
                    || playback.session_seek_prepared.load(Ordering::SeqCst) != revision
                {
                    return Err(PlayError::SessionSeekPreparationFailed);
                }
            }
            let player_ids = participants
                .iter()
                .map(|(player_id, _)| *player_id)
                .collect();
            self.core
                .engine
                .seek_session_checked(target, snapshot.revision(), shape, player_ids)
        }
        .await;
        if let Err(error) = transaction {
            if preparation_started {
                cancel_session_seek(&participants, &slots, revision)?;
            }
            return Err(error);
        }
        Ok(())
    }

    fn session_participants<'a>(
        &'a self,
        peers: &[&'a Self],
    ) -> Result<Vec<(PlayerId, &'a Self)>, PlayError> {
        if peers
            .iter()
            .any(|peer| !self.core.engine.shares_session_with(&peer.core.engine))
        {
            return Err(PlayError::SessionMismatch);
        }
        let mut participants = std::iter::once(self)
            .chain(peers.iter().copied())
            .map(|player| {
                player
                    .core
                    .engine
                    .player_id()
                    .map(|player_id| (player_id, player))
                    .ok_or(PlayError::NotReady)
            })
            .collect::<Result<Vec<_>, _>>()?;
        participants.sort_unstable_by_key(|(player_id, _)| *player_id);
        participants.dedup_by_key(|(player_id, _)| *player_id);
        Ok(participants)
    }
}

fn cancel_session_seek(
    participants: &[(PlayerId, &PlayerImpl)],
    slots: &[SlotId],
    revision: u64,
) -> Result<(), PlayError> {
    let mut first_error = None;
    for ((_, player), slot) in participants.iter().zip(slots) {
        if let Err(rejected) = player
            .core
            .engine
            .try_send_slot_cmd(*slot, PlayerCmd::CancelSessionSeek { revision })
            && first_error.is_none()
        {
            first_error = Some(rejected.error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

impl SessionTrackControl for PlayerImpl {
    fn join_track_at<'a>(
        &'a self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> impl Future<Output = Result<(), PlayError>> + 'a {
        Self::join_track_at(self, resource, item_id, binding, target)
    }

    fn seek_session<'a>(
        &'a self,
        peers: &'a [&'a Self],
        target: SessionBeat,
    ) -> impl Future<Output = Result<(), PlayError>> + 'a {
        Self::seek_session(self, peers, target)
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{player::PlayerConfig, session::testing};

    fn fill_slot_lane(player: &PlayerImpl, slot: SlotId) -> usize {
        let mut accepted = 0;
        while player
            .core
            .engine
            .try_send_slot_cmd(slot, PlayerCmd::SetPaused(false))
            .is_ok()
        {
            accepted += 1;
        }
        accepted
    }

    #[kithara::test]
    fn session_seek_cancellation_reaches_later_lanes_after_rejection() {
        let session = testing::test_session();
        let players: Vec<_> = (0..3)
            .map(|_| {
                let config = PlayerConfig {
                    session: Some(Arc::clone(&session)),
                    ..PlayerConfig::default()
                };
                PlayerImpl::new(config)
            })
            .collect();
        let slots: Vec<_> = players
            .iter()
            .map(|player| {
                player.ensure_engine_started().expect("engine starts");
                player.ensure_slot().expect("slot allocates")
            })
            .collect();
        let participants: Vec<_> = players[..2]
            .iter()
            .map(|player| {
                (
                    player.core.engine.player_id().expect("started player id"),
                    player,
                )
            })
            .collect();
        assert!(fill_slot_lane(&players[0], slots[0]) > 0);

        let error = cancel_session_seek(&participants, &slots[..2], 7)
            .expect_err("the saturated first lane rejects cancellation");

        assert!(matches!(
            error,
            PlayError::SlotChannelFull { slot } if slot == slots[0]
        ));
        let later_lane_remaining = fill_slot_lane(&players[1], slots[1]);
        let empty_lane_capacity = fill_slot_lane(&players[2], slots[2]);
        assert_eq!(later_lane_remaining + 1, empty_lane_capacity);
    }
}
