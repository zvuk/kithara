use kithara_audio::EqBandConfig;
use kithara_bufpool::PcmPool;

use super::state::{AllocatedSlot, Cmd, PlayerId, Reply};
use crate::{
    error::PlayError,
    types::{SessionDuckingMode, SlotId},
};

/// Object-safe view of the session: every operation `EngineImpl` invokes on
/// the audio session goes through `exec`. Production (cpal/web-audio,
/// async via the native engine thread) and per-instance offline
/// (synchronous, owned by a test harness) plug in interchangeably.
///
/// Typed methods are default-impl'd in terms of `exec`, so backends only
/// implement the dispatch primitive.
pub trait SessionDispatcher: Send + Sync + 'static {
    fn allocate_slot(&self, player_id: PlayerId) -> Result<AllocatedSlot, PlayError> {
        match self.exec_ok(Cmd::AllocateSlot { player_id })? {
            Reply::SlotAllocated(slot_id, cmd_tx, shared_state, eq) => {
                Ok((slot_id, cmd_tx, shared_state, eq))
            }
            _ => Err(PlayError::Internal(
                "unexpected reply for session allocate slot".into(),
            )),
        }
    }

    fn ducking(&self) -> Result<SessionDuckingMode, PlayError> {
        match self.exec_ok(Cmd::SessionDucking)? {
            Reply::SessionDucking(mode) => Ok(mode),
            _ => Err(PlayError::Internal(
                "unexpected reply for session ducking query".into(),
            )),
        }
    }

    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError>;

    fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let reply = self.exec(cmd)?;
        if let Reply::Err(msg) = &reply {
            return Err(PlayError::Internal(msg.clone()));
        }
        Ok(reply)
    }

    fn query_sample_rate(&self, fallback: u32) -> u32 {
        match self.exec(Cmd::QuerySampleRate) {
            Ok(Reply::SampleRate(sr)) => sr,
            _ => fallback,
        }
    }

    fn register_player(
        &self,
        eq_layout: Vec<EqBandConfig>,
        pcm_pool: PcmPool,
    ) -> Result<PlayerId, PlayError> {
        match self.exec_ok(Cmd::RegisterPlayer {
            eq_layout,
            pcm_pool,
        })? {
            Reply::PlayerRegistered(id) => Ok(id),
            _ => Err(PlayError::Internal(
                "unexpected reply for session player registration".into(),
            )),
        }
    }

    fn release_slot(&self, player_id: PlayerId, slot: SlotId) -> Result<(), PlayError> {
        self.exec_ok(Cmd::ReleaseSlot { player_id, slot })
            .map(|_| ())
    }

    fn set_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        self.exec_ok(Cmd::SetSessionDucking { mode }).map(|_| ())
    }

    fn set_player_eq_gain(
        &self,
        player_id: PlayerId,
        band: usize,
        gain_db: f32,
    ) -> Result<(), PlayError> {
        self.exec_ok(Cmd::SetPlayerEqGain {
            band,
            gain_db,
            player_id,
        })
        .map(|_| ())
    }

    fn set_player_master_volume(&self, player_id: PlayerId, volume: f32) -> Result<(), PlayError> {
        self.exec_ok(Cmd::SetPlayerMasterVolume { player_id, volume })
            .map(|_| ())
    }

    fn set_player_slot_volume(
        &self,
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    ) -> Result<(), PlayError> {
        self.exec_ok(Cmd::SetPlayerSlotVolume {
            player_id,
            slot,
            volume,
        })
        .map(|_| ())
    }

    fn start_player(
        &self,
        player_id: PlayerId,
        sample_rate: u32,
        master_volume: f32,
    ) -> Result<(), PlayError> {
        self.exec_ok(Cmd::StartPlayer {
            master_volume,
            player_id,
            sample_rate,
        })
        .map(|_| ())
    }

    fn stop_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
        self.exec_ok(Cmd::StopPlayer { player_id }).map(|_| ())
    }

    fn tick(&self) -> Result<(), PlayError> {
        self.exec_ok(Cmd::Tick).map(|_| ())
    }

    fn unregister_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
        self.exec_ok(Cmd::UnregisterPlayer { player_id })
            .map(|_| ())
    }
}
