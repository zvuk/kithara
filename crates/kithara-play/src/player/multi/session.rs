use kithara_platform::{
    CancelToken,
    sync::Arc,
    time::{Duration, sleep},
};

use super::super::core::PlayerImpl;
use crate::{
    PlayerId,
    api::{SessionBeat, SlotId, Tempo, TrackBinding, TransportRevision},
    bridge::{PlaybackShared, PlayerCmd, SessionSeekAttempt},
    error::PlayError,
    session::{SessionError, protocol::PreparationContext},
};

struct PreparedSeekParticipant<'a> {
    player: &'a PlayerImpl,
    playback: Arc<PlaybackShared>,
    player_id: PlayerId,
    attempt: SessionSeekAttempt,
    slot: SlotId,
    binding: TrackBinding,
    current_index: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SeekPreparationStatus {
    Failed,
    Pending,
    Prepared,
}

#[derive(Clone, Copy)]
struct SessionSeekPlan {
    context: PreparationContext,
    target: SessionBeat,
}

impl SessionSeekPlan {
    fn revision(self) -> Result<TransportRevision, SessionError> {
        self.context
            .transport_revision()
            .checked_next()
            .ok_or(SessionError::TransportRevisionExhausted)
    }
}

pub(super) fn set_session_tempo(players: &[&PlayerImpl], tempo: Tempo) -> Result<(), PlayError> {
    let participants = active_session_participants(players)?;
    let playlists: Vec<_> = players
        .iter()
        .map(|player| player.core.items.lock_playlist())
        .collect();
    let owner = participants
        .first()
        .map(|(_, player)| *player)
        .ok_or(PlayError::NotReady)?;
    let snapshot = owner.core.engine.session_transport()?;
    let context = owner.core.engine.preparation_context()?;
    if context.transport_revision() != snapshot.revision() || context.tempo() != snapshot.tempo() {
        return Err(PlayError::BindingPreparationContextChanged);
    }
    let shape = context.shape();
    for (player, playlist) in players.iter().zip(&playlists) {
        if is_active_session_player(player) {
            player.validate_session_tempo(snapshot, tempo, shape, playlist.current_binding())?;
        }
        PlayerImpl::validate_successor_tempo(playlist, tempo, shape)?;
    }
    let player_ids = participants
        .iter()
        .map(|(player_id, _)| *player_id)
        .collect();
    owner
        .core
        .engine
        .set_session_tempo_checked(tempo, context, player_ids)
}

fn active_session_participants<'a>(
    players: &[&'a PlayerImpl],
) -> Result<Vec<(PlayerId, &'a PlayerImpl)>, PlayError> {
    validate_shared_session(players)?;
    let mut participants = players
        .iter()
        .copied()
        .filter(|player| is_active_session_player(player))
        .map(|player| {
            player
                .core
                .engine
                .player_id()
                .map(|player_id| (player_id, player))
                .ok_or(PlayError::NotReady)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if participants.is_empty() {
        return Err(PlayError::NotReady);
    }
    participants.sort_unstable_by_key(|(player_id, _)| *player_id);
    participants.dedup_by_key(|(player_id, _)| *player_id);
    Ok(participants)
}

fn is_active_session_player(player: &PlayerImpl) -> bool {
    player.core.engine.is_running() && player.slot().is_some()
}

pub(super) async fn seek_session(
    players: &[&PlayerImpl],
    target: SessionBeat,
) -> Result<(), PlayError> {
    let participants = active_session_participants(players)?;
    let owner = participants
        .first()
        .map(|(_, player)| *player)
        .ok_or(PlayError::NotReady)?;
    let cancel = owner
        .core
        .engine
        .cancel_token()
        .ok_or_else(|| PlayError::Internal("session seek has no cancel owner".into()))?
        .child();
    let snapshot = owner.core.engine.session_transport()?;
    let context = owner.core.engine.preparation_context()?;
    if context.transport_revision() != snapshot.revision() || context.tempo() != snapshot.tempo() {
        return Err(PlayError::BindingPreparationContextChanged);
    }
    let plan = SessionSeekPlan { context, target };
    let prepared = prepare_session_seek_participants(&participants, plan)?;
    let transaction = async {
        dispatch_session_seek(&prepared, plan)?;
        wait_for_session_seek(&prepared, owner, &cancel, context).await?;
        validate_prepared_session_seek(&prepared, plan)?;
        if owner.core.engine.preparation_context()? != context {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        let player_ids = prepared
            .iter()
            .map(|participant| participant.player_id)
            .collect();
        owner
            .core
            .engine
            .seek_session_checked(target, context, player_ids)
    }
    .await;

    if let Err(error) = transaction {
        cancel_session_seek(
            prepared
                .iter()
                .map(|participant| (participant.playback.as_ref(), participant.attempt)),
        );
        return Err(error);
    }
    Ok(())
}

fn prepare_session_seek_participants<'a>(
    participants: &[(PlayerId, &'a PlayerImpl)],
    plan: SessionSeekPlan,
) -> Result<Vec<PreparedSeekParticipant<'a>>, PlayError> {
    let revision = plan.revision()?;
    let playlists: Vec<_> = participants
        .iter()
        .map(|(_, player)| player.core.items.lock_playlist())
        .collect();
    let mut prepared = Vec::with_capacity(participants.len());
    for ((player_id, player), playlist) in participants.iter().zip(&playlists) {
        let binding = playlist
            .current_binding()
            .cloned()
            .ok_or(PlayError::SessionSeekRequiresBoundTrack)?;
        player.validate_session_seek(
            plan.target,
            plan.context.tempo(),
            revision,
            plan.context.shape(),
            Some(&binding),
        )?;
        let slot = player.slot().ok_or(PlayError::NoActiveSlot)?;
        let playback = player
            .core
            .engine
            .slot_playback(slot)
            .ok_or(PlayError::SlotNotFound(slot))?;
        let attempt = playback
            .next_session_seek_attempt()
            .ok_or(PlayError::SessionSeekAttemptExhausted)?;
        prepared.push(PreparedSeekParticipant {
            attempt,
            binding,
            playback,
            player,
            slot,
            current_index: playlist.current(),
            player_id: *player_id,
        });
    }
    Ok(prepared)
}

fn dispatch_session_seek(
    prepared: &[PreparedSeekParticipant<'_>],
    plan: SessionSeekPlan,
) -> Result<(), PlayError> {
    let revision = plan.revision()?;
    for participant in prepared {
        participant
            .player
            .core
            .engine
            .try_send_slot_cmd(
                participant.slot,
                PlayerCmd::PrepareSessionSeek {
                    revision,
                    attempt: participant.attempt,
                    target: plan.target,
                    tempo: plan.context.tempo(),
                },
            )
            .map_err(|rejected| rejected.error)?;
    }
    Ok(())
}

async fn wait_for_session_seek(
    prepared: &[PreparedSeekParticipant<'_>],
    owner: &PlayerImpl,
    cancel: &CancelToken,
    context: PreparationContext,
) -> Result<(), PlayError> {
    loop {
        if cancel.is_cancelled() {
            return Err(PlayError::BindingPreparationCancelled);
        }
        match session_seek_status(prepared) {
            SeekPreparationStatus::Failed => {
                return Err(PlayError::SessionSeekPreparationFailed);
            }
            SeekPreparationStatus::Prepared => return Ok(()),
            SeekPreparationStatus::Pending => {}
        }
        if owner.core.engine.preparation_context()? != context {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        sleep(Duration::from_millis(1)).await;
    }
}

fn session_seek_status(prepared: &[PreparedSeekParticipant<'_>]) -> SeekPreparationStatus {
    let mut status = SeekPreparationStatus::Prepared;
    for participant in prepared {
        if participant
            .playback
            .session_seek_failed(participant.attempt)
        {
            return SeekPreparationStatus::Failed;
        }
        if !participant
            .playback
            .session_seek_prepared(participant.attempt)
        {
            status = SeekPreparationStatus::Pending;
        }
    }
    status
}

fn validate_prepared_session_seek(
    prepared: &[PreparedSeekParticipant<'_>],
    plan: SessionSeekPlan,
) -> Result<(), PlayError> {
    let revision = plan.revision()?;
    let playlists: Vec<_> = prepared
        .iter()
        .map(|participant| participant.player.core.items.lock_playlist())
        .collect();
    for (participant, playlist) in prepared.iter().zip(&playlists) {
        if participant.player.slot() != Some(participant.slot)
            || playlist.current() != participant.current_index
            || playlist.current_binding() != Some(&participant.binding)
        {
            return Err(PlayError::BindingPreparationContextChanged);
        }
        participant.player.validate_session_seek(
            plan.target,
            plan.context.tempo(),
            revision,
            plan.context.shape(),
            playlist.current_binding(),
        )?;
        if participant
            .playback
            .session_seek_failed(participant.attempt)
            || !participant
                .playback
                .session_seek_prepared(participant.attempt)
        {
            return Err(PlayError::SessionSeekPreparationFailed);
        }
    }
    Ok(())
}

fn cancel_session_seek<'a>(
    participants: impl IntoIterator<Item = (&'a PlaybackShared, SessionSeekAttempt)>,
) {
    for (playback, attempt) in participants {
        playback.cancel_session_seek(attempt);
    }
}

fn validate_shared_session(players: &[&PlayerImpl]) -> Result<(), PlayError> {
    let owner = players.first().copied().ok_or(PlayError::NotReady)?;
    if players
        .iter()
        .any(|player| !owner.core.engine.shares_session_with(&player.core.engine))
    {
        return Err(PlayError::SessionMismatch);
    }
    Ok(())
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
    fn session_seek_cancellation_is_not_blocked_by_saturated_lanes() {
        let session = testing::test_session();
        let players: Vec<_> = (0..2)
            .map(|_| {
                PlayerImpl::new(PlayerConfig {
                    session: Some(Arc::clone(&session)),
                    ..PlayerConfig::default()
                })
            })
            .collect();
        let slots: Vec<_> = players
            .iter()
            .map(|player| {
                player.ensure_engine_started().expect("engine starts");
                player.ensure_slot().expect("slot allocates")
            })
            .collect();
        assert!(fill_slot_lane(&players[0], slots[0]) > 0);
        let playback: Vec<_> = players
            .iter()
            .zip(&slots)
            .map(|(player, slot)| {
                player
                    .core
                    .engine
                    .slot_playback(*slot)
                    .expect("slot owns playback state")
            })
            .collect();
        let attempts: Vec<_> = playback
            .iter()
            .map(|state| {
                state
                    .next_session_seek_attempt()
                    .expect("test attempt allocates")
            })
            .collect();

        cancel_session_seek(
            playback
                .iter()
                .zip(&attempts)
                .map(|(state, attempt)| (state.as_ref(), *attempt)),
        );

        assert!(
            playback
                .iter()
                .zip(&attempts)
                .all(|(state, attempt)| state.session_seek_cancelled(*attempt))
        );
    }

    #[kithara::test]
    fn active_participants_exclude_registered_but_unstarted_decks() {
        let session = testing::test_session();
        let active = PlayerImpl::new(PlayerConfig {
            session: Some(Arc::clone(&session)),
            ..PlayerConfig::default()
        });
        let idle = PlayerImpl::new(PlayerConfig {
            session: Some(session),
            ..PlayerConfig::default()
        });
        active.ensure_engine_started().expect("engine starts");
        active.ensure_slot().expect("slot allocates");

        let participants = active_session_participants(&[&active, &idle]).expect("sessions match");

        assert_eq!(participants.len(), 1);
        assert_eq!(participants[0].1.slot(), active.slot());
    }
}
