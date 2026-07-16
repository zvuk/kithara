use firewheel::{FirewheelCtx, backend::AudioBackend, error::UpdateError};
use tracing::{debug, trace, warn};

use super::{
    graph::{controls, lifecycle, player_index, slots},
    protocol::{Cmd, PlayerId, Reply, SessionError},
    state::{SessionState, register_player},
    transport,
};

pub fn run_cmd<B: AudioBackend>(state: &mut SessionState<B>, cmd: Cmd) -> Reply {
    match cmd {
        Cmd::RegisterPlayer {
            eq_layout,
            pcm_pool,
        } => Reply::PlayerRegistered(register_player(state, eq_layout, pcm_pool)),
        Cmd::UnregisterPlayer { player_id } => match unregister_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StartPlayer {
            master_volume,
            player_id,
            sample_rate,
        } => match lifecycle::start_player(state, player_id, sample_rate, master_volume) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StopPlayer { player_id } => match lifecycle::stop_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::AllocateSlot { player_id } => match slots::allocate_slot(state, player_id) {
            Ok(reply) => reply,
            Err(err) => Reply::Err(err),
        },
        Cmd::ReleaseSlot { player_id, slot } => match slots::release_slot(state, player_id, slot) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerMasterVolume { player_id, volume } => {
            match controls::set_player_master_volume(state, player_id, volume) {
                Ok(()) => Reply::Ok,
                Err(err) => Reply::Err(err),
            }
        }
        Cmd::SetPlayerSlotVolume {
            player_id,
            slot,
            volume,
        } => match controls::set_player_slot_volume(state, player_id, slot, volume) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerEqGain {
            band,
            gain_db,
            player_id,
        } => match controls::set_player_eq_gain(state, player_id, band, gain_db) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetSessionDucking { mode } => {
            controls::set_session_ducking(state, mode);
            Reply::Ok
        }
        Cmd::SessionDucking => Reply::SessionDucking(state.session_ducking),
        Cmd::SetSessionTempo { tempo } => match transport::set_tempo(state, tempo) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SessionTransport => match transport::snapshot(state) {
            Ok(snapshot) => Reply::SessionTransport(snapshot),
            Err(err) => Reply::Err(err),
        },
        Cmd::InvalidateAudioRoute { reason } => invalidate_audio_route(state, &reason),
        Cmd::QuerySampleRate => {
            let sample_rate = state
                .ctx
                .as_ref()
                .and_then(FirewheelCtx::stream_info)
                .map_or(state.sample_rate_hint, |si| si.sample_rate.get());
            trace_stream_info(state, "query-sample-rate");
            Reply::SampleRate(sample_rate)
        }
        Cmd::Tick => tick_session(state),
    }
}

pub(super) fn tick_session<B: AudioBackend>(state: &mut SessionState<B>) -> Reply {
    if state.stream_needs_restart {
        match restart_stream(state, state.sample_rate_hint) {
            Ok(()) => {}
            Err(err) => {
                warn!(?err, "[KITHARA-ROUTE] deferred stream restart failed");
                return Reply::Err(SessionError::RestartFailed {
                    reason: "deferred stream restart".into(),
                    r#source: err.to_string(),
                });
            }
        }
    }

    let update = state.ctx.as_mut().map(FirewheelCtx::update);
    if let Some(Err(err)) = update {
        return handle_update_error(state, err);
    }
    Reply::Ok
}

fn unregister_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    player_id: PlayerId,
) -> Result<(), SessionError> {
    debug!(player_id, "[KITHARA-ROUTE] unregistering player");
    let idx = player_index(state, player_id)?;
    if state.players[idx].started {
        lifecycle::stop_player(state, player_id)?;
    }
    state.players.remove(idx);
    debug!(
        player_id,
        players = state.players.len(),
        "[KITHARA-ROUTE] player unregistered"
    );
    Ok(())
}

pub(super) fn handle_update_error<B: AudioBackend>(
    state: &mut SessionState<B>,
    err: UpdateError<B::StreamError>,
) -> Reply {
    match err {
        UpdateError::StreamStoppedUnexpectedly(reason) => {
            state.stream_needs_restart = true;
            warn!(
                ?reason,
                "session stream stopped unexpectedly; restarting audio stream"
            );
            trace!(
                ?reason,
                sample_rate_hint = state.sample_rate_hint,
                "[KITHARA-ROUTE] firewheel update reported stopped stream"
            );
            match restart_stream(state, state.sample_rate_hint) {
                Ok(()) => Reply::Ok,
                Err(restart_err) => Reply::Err(SessionError::RestartFailed {
                    reason: format!("{reason:?}"),
                    r#source: restart_err.to_string(),
                }),
            }
        }
        other => {
            warn!(?other, "[KITHARA-ROUTE] firewheel update failed");
            Reply::Err(SessionError::Graph(format!("{other:?}")))
        }
    }
}

pub(super) fn invalidate_audio_route<B: AudioBackend>(
    state: &mut SessionState<B>,
    reason: &str,
) -> Reply {
    debug!(
        reason,
        ctx_ready = state.ctx.is_some(),
        stream_needs_restart = state.stream_needs_restart,
        "[KITHARA-ROUTE] audio route invalidated"
    );
    if state.ctx.is_none() {
        return Reply::Ok;
    }
    state.stream_needs_restart = true;
    match restart_stream(state, state.sample_rate_hint) {
        Ok(()) => Reply::Ok,
        Err(err) => Reply::Err(SessionError::RestartFailed {
            reason: reason.to_owned(),
            r#source: err.to_string(),
        }),
    }
}

pub(super) fn restart_stream<B: AudioBackend>(
    state: &mut SessionState<B>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err(SessionError::NoContext);
    };
    debug!(sample_rate, "[KITHARA-ROUTE] restarting firewheel stream");
    if fw_ctx.is_audio_stream_running() {
        trace!("[KITHARA-ROUTE] stopping existing stream before restart");
        fw_ctx.stop_stream();
    }
    (state.start_stream_fn)(fw_ctx, sample_rate).map_err(SessionError::StreamStart)?;
    state.sample_rate_hint = sample_rate;
    state.stream_needs_restart = false;
    trace_stream_info(state, "restart-stream");
    debug!(
        sample_rate,
        "[KITHARA-ROUTE] firewheel stream restart complete"
    );
    Ok(())
}

pub(super) fn trace_stream_info<B: AudioBackend>(state: &SessionState<B>, context: &'static str) {
    if let Some(info) = state.ctx.as_ref().and_then(FirewheelCtx::stream_info) {
        trace!(
            context,
            sample_rate = info.sample_rate.get(),
            prev_sample_rate = info.prev_sample_rate.get(),
            max_block_frames = info.max_block_frames.get(),
            out_channels = info.num_stream_out_channels,
            output_device_id = %info.output_device_id,
            input_device_id = ?info.input_device_id.as_deref(),
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info"
        );
    } else {
        trace!(
            context,
            sample_rate_hint = state.sample_rate_hint,
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info unavailable"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use firewheel::{FirewheelCtx, StreamInfo, processor::FirewheelProcessor};
    use kithara_bufpool::PcmPool;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::session::{
        protocol::{Cmd, Reply, SessionError},
        state::SessionState,
    };

    #[derive(Default)]
    struct RouteLossProbe {
        fail_next_poll: AtomicBool,
        fail_next_start: AtomicBool,
        start_count: AtomicUsize,
    }

    impl RouteLossProbe {
        fn reset(&self) {
            self.start_count.store(0, Ordering::SeqCst);
            self.fail_next_poll.store(false, Ordering::SeqCst);
            self.fail_next_start.store(false, Ordering::SeqCst);
        }
    }

    thread_local! {
        static ROUTE_LOSS: RouteLossProbe = RouteLossProbe::default();
    }

    fn route_loss<R>(f: impl FnOnce(&RouteLossProbe) -> R) -> R {
        ROUTE_LOSS.with(f)
    }

    struct RouteLossBackend {
        _processor: Option<FirewheelProcessor<Self>>,
    }

    #[derive(Clone)]
    struct RouteLossConfig {
        sample_rate: u32,
    }

    impl Default for RouteLossConfig {
        fn default() -> Self {
            Self {
                sample_rate: SessionState::<RouteLossBackend>::DEFAULT_SAMPLE_RATE,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("route lost")]
    struct RouteLossError;

    impl AudioBackend for RouteLossBackend {
        type Config = RouteLossConfig;
        type Enumerator = ();
        type Instant = kithara_platform::time::Instant;
        type StartStreamError = RouteLossError;
        type StreamError = RouteLossError;

        fn delay_from_last_process(
            &self,
            _process_timestamp: Self::Instant,
        ) -> Option<kithara_platform::time::Duration> {
            None
        }

        fn enumerator() -> Self::Enumerator {}

        fn poll_status(&mut self) -> Result<(), Self::StreamError> {
            if route_loss(|probe| probe.fail_next_poll.swap(false, Ordering::SeqCst)) {
                Err(RouteLossError)
            } else {
                Ok(())
            }
        }

        fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
            self._processor = Some(processor);
        }

        fn start_stream(
            config: Self::Config,
        ) -> Result<(Self, StreamInfo), Self::StartStreamError> {
            route_loss(|probe| probe.start_count.fetch_add(1, Ordering::SeqCst));
            if route_loss(|probe| probe.fail_next_start.swap(false, Ordering::SeqCst)) {
                return Err(RouteLossError);
            }

            let sample_rate = NonZeroU32::new(config.sample_rate).ok_or(RouteLossError)?;
            let max_block_frames = NonZeroU32::new(512).ok_or(RouteLossError)?;
            let stream_info = StreamInfo {
                sample_rate,
                sample_rate_recip: 1.0 / f64::from(config.sample_rate),
                prev_sample_rate: sample_rate,
                max_block_frames,
                num_stream_in_channels: 0,
                num_stream_out_channels: 2,
                input_to_output_latency_seconds: 0.0,
                declick_frames: max_block_frames,
                output_device_id: String::from("route-loss-test"),
                input_device_id: None,
            };
            Ok((Self { _processor: None }, stream_info))
        }
    }

    fn start_route_loss_stream(
        ctx: &mut FirewheelCtx<RouteLossBackend>,
        sample_rate: u32,
    ) -> Result<(), String> {
        ctx.start_stream(RouteLossConfig { sample_rate })
            .map_err(|err| err.to_string())
    }

    fn register_player(state: &mut SessionState<RouteLossBackend>) -> u64 {
        match run_cmd(
            state,
            Cmd::RegisterPlayer {
                eq_layout: Vec::new(),
                pcm_pool: PcmPool::default().clone(),
            },
        ) {
            Reply::PlayerRegistered(id) => id,
            Reply::Err(err) => panic!("player registration failed: {err}"),
            _ => panic!("player registration returned unexpected reply"),
        }
    }

    #[kithara::test]
    fn explicit_audio_route_invalidation_restarts_stream_without_backend_error() {
        route_loss(RouteLossProbe::reset);

        let mut state = SessionState::<RouteLossBackend>::new(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::StartPlayer {
                    player_id,
                    sample_rate: 44_100,
                    master_volume: 1.0,
                },
            ),
            Reply::Ok
        ));
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::InvalidateAudioRoute {
                    reason: String::from("oldDeviceUnavailable"),
                },
            ),
            Reply::Ok
        ));

        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2,
            "explicit platform route invalidation must restart the audio stream"
        );
        assert!(
            state.ctx.is_some(),
            "route invalidation must keep the graph context"
        );
        assert!(
            state.players[0].started,
            "route invalidation must keep the player graph logically started"
        );
        assert_eq!(
            state.players[0].slots.len(),
            1,
            "route invalidation must not drop active slots"
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            state.players[0].slots.len(),
            2,
            "session must accept future slots after explicit route restart"
        );
        assert!(!state.stream_needs_restart);
    }

    #[kithara::test]
    fn unexpected_stream_stop_restarts_stream_without_dropping_player_graph_or_future_slots() {
        route_loss(RouteLossProbe::reset);

        let mut state = SessionState::<RouteLossBackend>::new(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::StartPlayer {
                    player_id,
                    sample_rate: 44_100,
                    master_volume: 1.0,
                },
            ),
            Reply::Ok
        ));
        assert!(state.ctx.is_some());
        assert!(state.players[0].started);
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(state.players[0].slots.len(), 1);

        route_loss(|probe| probe.fail_next_poll.store(true, Ordering::SeqCst));
        assert!(matches!(run_cmd(&mut state, Cmd::Tick), Reply::Ok));

        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2,
            "stream loss must restart the audio stream immediately"
        );
        assert!(
            state.ctx.is_some(),
            "session must keep the graph context across stream restart"
        );
        assert!(
            state.session_output_node_id.is_some(),
            "session output node id must survive stream restart"
        );
        assert!(
            state.players[0].started,
            "player graph must remain logically started after stream restart"
        );
        assert_eq!(
            state.players[0].slots.len(),
            1,
            "active slot graph must survive stream restart"
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            state.players[0].slots.len(),
            2,
            "session must accept a future slot after route-loss reinit"
        );
        assert!(!state.stream_needs_restart);
    }

    #[kithara::test]
    fn failed_stream_restart_is_retried_on_next_tick() {
        route_loss(RouteLossProbe::reset);

        let mut state = SessionState::<RouteLossBackend>::new(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::StartPlayer {
                    player_id,
                    sample_rate: 44_100,
                    master_volume: 1.0,
                },
            ),
            Reply::Ok
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );

        route_loss(|probe| {
            probe.fail_next_poll.store(true, Ordering::SeqCst);
            probe.fail_next_start.store(true, Ordering::SeqCst);
        });
        match run_cmd(&mut state, Cmd::Tick) {
            Reply::Err(err) => assert!(
                matches!(err, SessionError::RestartFailed { .. }),
                "restart failure must be surfaced, got {err:?}"
            ),
            _ => panic!("failed restart must return Reply::Err"),
        }

        assert!(
            state.stream_needs_restart,
            "a failed restart must leave retry state armed"
        );
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2
        );

        assert!(matches!(run_cmd(&mut state, Cmd::Tick), Reply::Ok));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            3,
            "next tick must retry the stream restart"
        );
        assert!(!state.stream_needs_restart);
        assert!(state.players[0].started);
    }
}
