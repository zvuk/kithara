use super::super::core::PlayerImpl;
use crate::{PlayerId, api::Tempo, error::PlayError};

impl PlayerImpl {
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

    /// Changes the shared session tempo for this player and its participating peers.
    pub fn set_session_tempo(&self, peers: &[&Self], tempo: Tempo) -> Result<(), PlayError> {
        let participants = self.session_participants(peers)?;
        let playlists: Vec<_> = participants
            .iter()
            .map(|(_, player)| player.core.items.lock_playlist())
            .collect();
        let snapshot = self.core.engine.session_transport()?;
        let context = self.core.engine.preparation_context()?;
        if context.transport_revision() != snapshot.revision()
            || context.tempo() != snapshot.tempo()
        {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        let shape = context.shape();
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
            .set_session_tempo_checked(tempo, context, player_ids)
    }
}
