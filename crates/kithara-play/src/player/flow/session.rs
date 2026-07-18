use std::{future::Future, sync::atomic::Ordering};

use kithara_platform::{
    sync::Arc,
    time::{Duration, sleep},
};

use super::super::core::PlayerImpl;
use crate::{
    PlayerId,
    api::{SessionBeat, Tempo, TrackBinding},
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
                for ((_, player), slot) in participants.iter().zip(&slots) {
                    player
                        .core
                        .engine
                        .try_send_slot_cmd(*slot, PlayerCmd::CancelSessionSeek { revision })
                        .map_err(|rejected| rejected.error)?;
                }
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
        let mut participants = Vec::with_capacity(peers.len() + 1);
        participants.push((
            self.core.engine.player_id().ok_or(PlayError::NotReady)?,
            self,
        ));
        for peer in peers {
            participants.push((
                peer.core.engine.player_id().ok_or(PlayError::NotReady)?,
                *peer,
            ));
        }
        participants.sort_unstable_by_key(|(player_id, _)| *player_id);
        participants.dedup_by_key(|(player_id, _)| *player_id);
        Ok(participants)
    }
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
