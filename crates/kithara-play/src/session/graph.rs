use firewheel::{
    FirewheelCtx, Volume, backend::AudioBackend, diff::Memo,
    dsp::volume::amp_to_linear_volume_clamped, node::NodeID, nodes::volume_pan::VolumePanNode,
};
use kithara_audio::EqBandConfig;
use tracing::{debug, warn};

use super::{
    protocol::{AllocatedSlot, PlayerId, PlayerLevel, Reply, SessionError},
    state::{PlayerState, SessionState, SlotNodes, ensure_ctx, prepare_eq_layout},
};
use crate::{
    api::{SessionDuckingMode, SlotId},
    bridge::slot_channels,
    rt::{MasterEqNode, PlayerNode},
};
pub(super) fn ducking_gain(mode: SessionDuckingMode) -> f32 {
    match mode {
        SessionDuckingMode::Off => 1.0,
        SessionDuckingMode::Soft => 0.4,
        SessionDuckingMode::Hard => 0.2,
    }
}
// A level is a linear amplitude, but `Volume::Linear` is a fader taper that
// squares its argument, so it must be converted rather than passed through.
pub(super) fn master_gain(level: f32) -> Volume {
    Volume::Linear(amp_to_linear_volume_clamped(level, 0.0))
}
pub(super) fn player_index<B: AudioBackend>(
    state: &SessionState<B>,
    player_id: PlayerId,
) -> Result<usize, SessionError> {
    state
        .players
        .iter()
        .position(|player| player.player_id == player_id)
        .ok_or(SessionError::PlayerNotFound(player_id))
}
fn graph_state(message: &'static str) -> SessionError {
    SessionError::Graph(message.into())
}
fn connect_stereo<B: AudioBackend>(
    fw_ctx: &mut FirewheelCtx<B>,
    from: NodeID,
    to: NodeID,
    label: &'static str,
) -> Result<(), SessionError> {
    fw_ctx
        .connect(from, to, &[(0, 0), (1, 1)], false)
        .map(|_| ())
        .map_err(|err| SessionError::Graph(format!("{label} failed: {err}")))
}

pub(super) mod lifecycle {
    use super::*;

    pub(in crate::session) fn start_player<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        sample_rate: u32,
        master_volume: f32,
    ) -> Result<(), SessionError> {
        debug!(
            player_id,
            sample_rate, master_volume, "[KITHARA-ROUTE] starting player"
        );
        ensure_ctx(state, sample_rate)?;
        let idx = player_index(state, player_id)?;
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(session_output_id) = state.session_output_node_id else {
            return Err(graph_state("session output node is not initialised"));
        };
        let player = &mut state.players[idx];
        if player.started {
            return Err(SessionError::AlreadyStarted(player_id));
        }
        let mut master_eq = MasterEqNode::new(&player.eq_layout);
        for (band, gain) in player.shared_eq.snapshot().into_iter().enumerate() {
            master_eq.set_gain(band, gain);
        }
        let master_eq_memo = Memo::new(master_eq.clone());
        let master_eq_id = fw_ctx.add_node(master_eq, None);
        player.master_volume = master_volume.clamp(0.0, 1.0);
        let master_vol = VolumePanNode::from_volume(master_gain(player.master_volume));
        let master_vol_memo = Memo::new(master_vol);
        let master_vol_id = fw_ctx.add_node(master_vol, None);
        let eq_to_volume = "connect player master_eq->master_vol";
        connect_stereo(fw_ctx, master_eq_id, master_vol_id, eq_to_volume)?;
        let volume_to_output = "connect player master_vol->session_output";
        connect_stereo(fw_ctx, master_vol_id, session_output_id, volume_to_output)?;
        if let Err(err) = fw_ctx.update() {
            warn!(player_id, "graph update after player start failed: {err:?}");
        }
        player.master_eq_node_id = Some(master_eq_id);
        player.master_eq_memo = Some(master_eq_memo);
        player.master_vol_pan_node_id = Some(master_vol_id);
        player.master_vol_pan_memo = Some(master_vol_memo);
        player.started = true;
        debug!(
            player_id,
            ?master_eq_id,
            ?master_vol_id,
            "[KITHARA-ROUTE] player graph started"
        );
        Ok(())
    }
    pub(in crate::session) fn stop_player<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
    ) -> Result<(), SessionError> {
        debug!(player_id, "[KITHARA-ROUTE] stopping player");
        let idx = player_index(state, player_id)?;
        stop_player_idx(state, idx)
    }
    fn stop_player_idx<B: AudioBackend>(
        state: &mut SessionState<B>,
        idx: usize,
    ) -> Result<(), SessionError> {
        if idx >= state.players.len() {
            return Err(graph_state("player index out of range"));
        }
        {
            let player = &mut state.players[idx];
            if !player.started {
                return Err(SessionError::NotRunning(player.player_id));
            }
            if let Some(ref mut fw_ctx) = state.ctx {
                remove_player_graph(fw_ctx, player);
                if let Err(err) = fw_ctx.update() {
                    warn!(
                        player_id = player.player_id,
                        "graph update after player stop failed: {err:?}"
                    );
                }
            } else {
                clear_player_graph_state(player);
            }
            player.started = false;
        }
        shutdown_if_idle(state);
        debug!("[KITHARA-ROUTE] player stopped");
        Ok(())
    }
    /// Release the output device once no player is left to feed it. A media
    /// app that has stopped playing must not keep the platform's output
    /// engaged; the next `start_player` builds a fresh context.
    fn shutdown_if_idle<B: AudioBackend>(state: &mut SessionState<B>) {
        if state.players.iter().all(|player| !player.started) {
            debug!("[KITHARA-ROUTE] shutting down idle session stream");
            if let Some(ref mut fw_ctx) = state.ctx {
                fw_ctx.stop_stream();
            }
            state.ctx = None;
            state.session_output_node_id = None;
            state.session_output_memo = None;
            state.session_limiter_node_id = None;
        }
    }
    pub(super) fn remove_player_graph<B: AudioBackend>(
        fw_ctx: &mut FirewheelCtx<B>,
        player: &mut PlayerState,
    ) {
        let player_id = player.player_id;
        for slot in player.slots.drain(..) {
            if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
                warn!(player_id, ?err, "failed to remove slot vol_pan node");
            }
            if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
                warn!(player_id, ?err, "failed to remove slot player node");
            }
        }
        if let Some(master_id) = player.master_vol_pan_node_id.take()
            && let Err(err) = fw_ctx.remove_node(master_id)
        {
            warn!(player_id, ?err, "failed to remove player master vol node");
        }
        if let Some(master_eq_id) = player.master_eq_node_id.take()
            && let Err(err) = fw_ctx.remove_node(master_eq_id)
        {
            warn!(player_id, ?err, "failed to remove player master eq node");
        }
        clear_player_graph_state(player);
    }
    pub(super) fn clear_player_graph_state(player: &mut PlayerState) {
        player.master_eq_memo = None;
        player.master_vol_pan_memo = None;
    }
}

pub(super) mod slots {
    use super::*;

    pub(in crate::session) fn allocate_slot<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
    ) -> Result<Reply, SessionError> {
        debug!(player_id, "[KITHARA-ROUTE] allocating player slot");
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let (fw_ctx, master_eq_id) = match (&mut state.ctx, state.players[idx].master_eq_node_id) {
            (None, _) => return Err(SessionError::NoContext),
            (Some(_), None) => return Err(graph_state("player master eq node is not initialised")),
            (Some(fw_ctx), Some(master_eq_id)) => (fw_ctx, master_eq_id),
        };
        let slot_id = SlotId::new(state.players[idx].next_slot_id);
        state.players[idx].next_slot_id += 1;
        let shared_eq = state.players[idx].shared_eq.clone();
        let (inputs, control) = slot_channels(shared_eq);
        let player_node = PlayerNode::new(inputs, state.players[idx].pcm_pool.clone());
        let player_node_id = fw_ctx.add_node(player_node, None);
        let slot_vol_pan = VolumePanNode::from_volume(Volume::Linear(1.0));
        let slot_vol_pan_memo = Memo::new(slot_vol_pan);
        let slot_vol_pan_id = fw_ctx.add_node(slot_vol_pan, None);
        let player_to_slot = "connect player->slot_vol_pan";
        connect_stereo(fw_ctx, player_node_id, slot_vol_pan_id, player_to_slot)?;
        let slot_to_master = "connect slot_vol_pan->player_master_eq";
        connect_stereo(fw_ctx, slot_vol_pan_id, master_eq_id, slot_to_master)?;
        if let Err(err) = fw_ctx.update() {
            warn!(
                player_id,
                ?slot_id,
                "graph update after slot allocate failed: {err:?}"
            );
        }
        state.players[idx].slots.push(SlotNodes {
            slot_id,
            player_node_id,
            vol_pan_memo: slot_vol_pan_memo,
            vol_pan_node_id: slot_vol_pan_id,
        });
        debug!(
            player_id,
            ?slot_id,
            ?player_node_id,
            ?slot_vol_pan_id,
            slots = state.players[idx].slots.len(),
            "[KITHARA-ROUTE] player slot allocated"
        );
        let reply = Reply::SlotAllocated(AllocatedSlot {
            slot: slot_id,
            control,
        });
        Ok(reply)
    }
    pub(in crate::session) fn release_slot<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        slot: SlotId,
    ) -> Result<(), SessionError> {
        debug!(player_id, ?slot, "[KITHARA-ROUTE] releasing player slot");
        let idx = player_index(state, player_id)?;
        let slot_nodes = {
            let player = &mut state.players[idx];
            if !player.started {
                return Err(SessionError::NotRunning(player_id));
            }
            take_slot(player, slot)?
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        remove_slot_graph(fw_ctx, player_id, &slot_nodes);
        debug!(
            player_id,
            ?slot_nodes,
            "[KITHARA-ROUTE] player slot released"
        );
        Ok(())
    }
    pub(super) fn take_slot(
        player: &mut PlayerState,
        slot: SlotId,
    ) -> Result<SlotNodes, SessionError> {
        let Some(slot_idx) = player.slots.iter().position(|s| s.slot_id == slot) else {
            return Err(SessionError::SlotNotFound(slot));
        };
        Ok(player.slots.remove(slot_idx))
    }
    pub(super) fn remove_slot_graph<B: AudioBackend>(
        fw_ctx: &mut FirewheelCtx<B>,
        player_id: PlayerId,
        slot: &SlotNodes,
    ) {
        if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
            warn!(player_id, ?err, "failed to remove slot vol_pan node");
        }
        if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
            warn!(player_id, ?err, "failed to remove slot player node");
        }
        if let Err(err) = fw_ctx.update() {
            warn!(player_id, "graph update after slot release failed: {err:?}");
        }
    }
}

pub(super) mod controls {
    use super::*;

    // Validates the whole request before mutating anything, so an invalid
    // entry leaves the batch untouched. Omitted players are unchanged.
    pub(in crate::session) fn set_player_master_volumes<B: AudioBackend>(
        state: &mut SessionState<B>,
        levels: &[PlayerLevel],
    ) -> Result<(), SessionError> {
        let mut resolved: Vec<(usize, f32)> = Vec::with_capacity(levels.len());
        for &PlayerLevel { player_id, level } in levels {
            if !level.is_finite() || !(0.0..=1.0).contains(&level) {
                return Err(SessionError::MasterVolumeOutOfRange { player_id, level });
            }
            let idx = player_index(state, player_id)?;
            if resolved.iter().any(|&(seen, _)| seen == idx) {
                return Err(SessionError::DuplicatePlayer(player_id));
            }
            // Checked here so the apply pass below is infallible
            // (all-or-nothing).
            let player = &state.players[idx];
            if player.started
                && (state.ctx.is_none()
                    || player.master_vol_pan_node_id.is_none()
                    || player.master_vol_pan_memo.is_none())
            {
                return Err(graph_state("player master vol graph is not initialised"));
            }
            resolved.push((idx, level));
        }
        for &(idx, level) in &resolved {
            apply_master_volume(state, idx, level);
        }
        Ok(())
    }

    fn apply_master_volume<B: AudioBackend>(state: &mut SessionState<B>, idx: usize, volume: f32) {
        state.players[idx].master_volume = volume;
        let player = &mut state.players[idx];
        if let (Some(fw_ctx), Some(master_id), Some(memo)) = (
            &mut state.ctx,
            player.master_vol_pan_node_id,
            &mut player.master_vol_pan_memo,
        ) {
            memo.volume = master_gain(volume);
            let mut queue = fw_ctx.event_queue(master_id);
            memo.update_memo(&mut queue);
        }
    }
    pub(in crate::session) fn set_player_slot_volume<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let Some(slot_nodes) = state.players[idx]
            .slots
            .iter_mut()
            .find(|s| s.slot_id == slot)
        else {
            return Err(SessionError::SlotNotFound(slot));
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        slot_nodes.vol_pan_memo.volume = Volume::Linear(volume.clamp(0.0, 1.0));
        let mut queue = fw_ctx.event_queue(slot_nodes.vol_pan_node_id);
        slot_nodes.vol_pan_memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::session) fn set_player_eq_gain<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        band: usize,
        gain_db: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let player = &mut state.players[idx];
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(master_eq_id) = player.master_eq_node_id else {
            return Err(graph_state("player master eq node is not initialised"));
        };
        let Some(memo) = &mut player.master_eq_memo else {
            return Err(graph_state("player master eq memo is not initialised"));
        };
        if band >= memo.bands.len() {
            return Err(SessionError::EqBandOutOfRange {
                band,
                bands: memo.bands.len(),
            });
        }
        memo.set_gain(band, gain_db);
        let mut queue = fw_ctx.event_queue(master_eq_id);
        memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::session) fn set_player_eq_layout<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        eq_layout: Vec<EqBandConfig>,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        let (eq_layout, gains) = prepare_eq_layout(eq_layout);
        if !state.players[idx].started {
            let player = &mut state.players[idx];
            player.eq_layout = eq_layout;
            player.shared_eq.replace(gains);
            return Ok(());
        }

        let (old_eq_id, master_volume_id, slot_volume_ids) = {
            let player = &state.players[idx];
            let old_eq_id = player
                .master_eq_node_id
                .ok_or_else(|| graph_state("player master eq node is not initialised"))?;
            let master_volume_id = player
                .master_vol_pan_node_id
                .ok_or_else(|| graph_state("player master vol node is not initialised"))?;
            let slot_volume_ids = player
                .slots
                .iter()
                .map(|slot| slot.vol_pan_node_id)
                .collect::<Vec<_>>();
            (old_eq_id, master_volume_id, slot_volume_ids)
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let master_eq = MasterEqNode::new(&eq_layout);
        let master_eq_memo = Memo::new(master_eq.clone());
        let master_eq_id = fw_ctx.add_node(master_eq, None);

        let connect_result = slot_volume_ids
            .into_iter()
            .try_for_each(|slot_id| {
                connect_stereo(
                    fw_ctx,
                    slot_id,
                    master_eq_id,
                    "connect slot_vol_pan->replacement master_eq",
                )
            })
            .and_then(|()| {
                connect_stereo(
                    fw_ctx,
                    master_eq_id,
                    master_volume_id,
                    "connect replacement master_eq->master_vol",
                )
            });
        if let Err(err) = connect_result {
            if let Err(remove_err) = fw_ctx.remove_node(master_eq_id) {
                warn!(
                    player_id,
                    ?remove_err,
                    "failed to remove rejected replacement EQ node"
                );
            }
            return Err(err);
        }
        if let Err(err) = fw_ctx.remove_node(old_eq_id) {
            if let Err(remove_err) = fw_ctx.remove_node(master_eq_id) {
                warn!(
                    player_id,
                    ?remove_err,
                    "failed to remove replacement EQ node after swap rejection"
                );
            }
            return Err(SessionError::Graph(format!(
                "remove previous master_eq failed: {err}"
            )));
        }
        if let Err(err) = fw_ctx.update() {
            warn!(
                player_id,
                "graph update after EQ layout swap failed: {err:?}"
            );
        }

        let player = &mut state.players[idx];
        player.eq_layout = eq_layout;
        player.shared_eq.replace(gains);
        player.master_eq_node_id = Some(master_eq_id);
        player.master_eq_memo = Some(master_eq_memo);
        Ok(())
    }
    pub(in crate::session) fn set_session_ducking<B: AudioBackend>(
        state: &mut SessionState<B>,
        mode: SessionDuckingMode,
    ) {
        state.session_ducking = mode;
        if let (Some(fw_ctx), Some(session_id), Some(memo)) = (
            &mut state.ctx,
            state.session_output_node_id,
            &mut state.session_output_memo,
        ) {
            memo.volume = Volume::Linear(ducking_gain(mode));
            let mut queue = fw_ctx.event_queue(session_id);
            memo.update_memo(&mut queue);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, num::NonZeroU32};

    use firewheel::{
        StreamInfo, backend::BackendProcessInfo, node::StreamStatus, processor::FirewheelProcessor,
    };
    use kithara_audio::generate_log_spaced_bands;
    use kithara_bufpool::PcmPool;
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::session::{dispatch::run_cmd, protocol::Cmd};

    const BLOCK_FRAMES: usize = 128;

    /// The process-wide output device, held by whichever stream owns it.
    #[derive(Default)]
    struct AudioDevice {
        processor: Option<FirewheelProcessor<TestBackend>>,
        owner: u64,
        next_stream: u64,
    }

    thread_local! {
        static DEVICE: RefCell<AudioDevice> = RefCell::new(AudioDevice::default());
    }

    fn device<R>(f: impl FnOnce(&mut AudioDevice) -> R) -> R {
        DEVICE.with(|cell| f(&mut cell.borrow_mut()))
    }

    struct TestBackend {
        stream: u64,
    }

    impl Drop for TestBackend {
        fn drop(&mut self) {
            device(|dev| {
                if dev.owner == self.stream {
                    dev.processor = None;
                }
            });
        }
    }

    #[derive(Clone)]
    struct TestConfig {
        sample_rate: u32,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                sample_rate: SessionState::<TestBackend>::DEFAULT_SAMPLE_RATE,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("test backend stream error")]
    struct TestBackendError;

    impl AudioBackend for TestBackend {
        type Config = TestConfig;
        type Enumerator = ();
        type Instant = Instant;
        type StartStreamError = TestBackendError;
        type StreamError = TestBackendError;

        fn delay_from_last_process(&self, _process_timestamp: Self::Instant) -> Option<Duration> {
            None
        }

        fn enumerator() -> Self::Enumerator {}

        fn poll_status(&mut self) -> Result<(), Self::StreamError> {
            Ok(())
        }

        fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
            device(|dev| {
                dev.owner = self.stream;
                dev.processor = Some(processor);
            });
        }

        fn start_stream(
            config: Self::Config,
        ) -> Result<(Self, StreamInfo), Self::StartStreamError> {
            let stream = device(|dev| {
                dev.next_stream += 1;
                dev.next_stream
            });
            let sample_rate = NonZeroU32::new(config.sample_rate).ok_or(TestBackendError)?;
            let max_block_frames = NonZeroU32::new(512).ok_or(TestBackendError)?;
            let stream_info = StreamInfo {
                sample_rate,
                sample_rate_recip: 1.0 / f64::from(config.sample_rate),
                prev_sample_rate: sample_rate,
                max_block_frames,
                num_stream_in_channels: 0,
                num_stream_out_channels: 2,
                input_to_output_latency_seconds: 0.0,
                declick_frames: max_block_frames,
                output_device_id: String::from("test-output-device"),
                input_device_id: None,
            };
            Ok((Self { stream }, stream_info))
        }
    }

    fn start_test_stream(
        ctx: &mut FirewheelCtx<TestBackend>,
        sample_rate: u32,
    ) -> Result<(), String> {
        ctx.start_stream(TestConfig { sample_rate })
            .map_err(|err| err.to_string())
    }

    /// `false` means no stream owns the device, which is what silence looks like.
    fn deliver_one_block() -> bool {
        device(|dev| {
            let Some(processor) = dev.processor.as_mut() else {
                return false;
            };
            let mut output = [0.0_f32; BLOCK_FRAMES * 2];
            processor.process_interleaved(
                &[],
                &mut output,
                BackendProcessInfo {
                    num_in_channels: 0,
                    num_out_channels: 2,
                    frames: BLOCK_FRAMES,
                    process_timestamp: Instant::now(),
                    duration_since_stream_start: Duration::ZERO,
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status: StreamStatus::empty(),
                    dropped_frames: 0,
                },
            );
            true
        })
    }

    fn processed_frames(state: &SessionState<TestBackend>) -> i64 {
        state
            .ctx
            .as_ref()
            .map_or(-1, |fw_ctx| fw_ctx.audio_clock().samples.0)
    }

    fn register(state: &mut SessionState<TestBackend>) -> PlayerId {
        match run_cmd(
            state,
            Cmd::RegisterPlayer {
                eq_layout: generate_log_spaced_bands(5),
                pcm_pool: PcmPool::default().clone(),
            },
        ) {
            Reply::PlayerRegistered(id) => id,
            Reply::Err(err) => panic!("player registration failed: {err}"),
            _ => panic!("player registration returned unexpected reply"),
        }
    }

    fn start(state: &mut SessionState<TestBackend>, player_id: PlayerId) {
        match run_cmd(
            state,
            Cmd::StartPlayer {
                player_id,
                sample_rate: SessionState::<TestBackend>::DEFAULT_SAMPLE_RATE,
                master_volume: 1.0,
            },
        ) {
            Reply::Ok => {}
            Reply::Err(err) => panic!("player {player_id} failed to start: {err}"),
            _ => panic!("player start returned unexpected reply"),
        }
    }

    fn unregister(state: &mut SessionState<TestBackend>, player_id: PlayerId) {
        match run_cmd(state, Cmd::UnregisterPlayer { player_id }) {
            Reply::Ok => {}
            Reply::Err(err) => panic!("player {player_id} failed to unregister: {err}"),
            _ => panic!("player unregister returned unexpected reply"),
        }
    }

    #[kithara::test]
    fn a_running_player_replaces_its_eq_layout_without_releasing_slots() {
        device(|dev| *dev = AudioDevice::default());
        let mut state = SessionState::<TestBackend>::new(start_test_stream);
        let player_id = register(&mut state);
        start(&mut state, player_id);
        let slot = match run_cmd(&mut state, Cmd::AllocateSlot { player_id }) {
            Reply::SlotAllocated(allocated) => allocated.slot,
            Reply::Err(err) => panic!("slot allocation failed: {err}"),
            _ => panic!("slot allocation returned unexpected reply"),
        };
        let previous_eq = state.players[0].master_eq_node_id;
        let previous_volume = state.players[0].master_vol_pan_node_id;
        let mut layout = generate_log_spaced_bands(4);
        for (band, gain) in layout.iter_mut().zip([-6.0, -3.0, 1.5, 4.0]) {
            band.set_gain_db(gain);
        }

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::SetPlayerEqLayout {
                    player_id,
                    eq_layout: layout,
                },
            ),
            Reply::Ok
        ));

        let player = &state.players[0];
        assert_eq!(player.eq_layout.len(), 4);
        assert_eq!(player.shared_eq.snapshot(), vec![-6.0, -3.0, 1.5, 4.0]);
        assert_eq!(player.slots.len(), 1);
        assert_eq!(player.slots[0].slot_id, slot);
        assert_ne!(player.master_eq_node_id, previous_eq);
        assert_eq!(player.master_vol_pan_node_id, previous_volume);
        assert_eq!(
            player.master_eq_memo.as_ref().map(|memo| memo.bands.len()),
            Some(4)
        );
        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::SetPlayerEqGain {
                    player_id,
                    band: 3,
                    gain_db: 5.0,
                },
            ),
            Reply::Ok
        ));
    }

    #[kithara::test]
    fn a_second_player_started_after_the_last_one_left_gets_a_processed_stream() {
        device(|dev| *dev = AudioDevice::default());
        let mut state = SessionState::<TestBackend>::new(start_test_stream);

        let first = register(&mut state);
        start(&mut state, first);
        assert!(
            deliver_one_block(),
            "the first player's stream must own the output device"
        );

        unregister(&mut state, first);

        assert!(
            state.ctx.is_none(),
            "the session must release the output device once no player feeds it"
        );

        let second = register(&mut state);
        start(&mut state, second);
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id: second }),
            Reply::SlotAllocated(..)
        ));

        let before = processed_frames(&state);
        assert!(
            deliver_one_block(),
            "the second player's stream must own the output device"
        );
        assert!(
            processed_frames(&state) > before,
            "the second player's stream delivered no processed callback"
        );
    }
}
