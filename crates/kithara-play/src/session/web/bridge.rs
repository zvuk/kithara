use std::{
    cell::RefCell,
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
};

use firewheel::FirewheelCtx;
use tracing::warn;

use super::client::{WASM_SESSION_STATE, worker};
use crate::impls::{
    session::{dispatch::run_cmd, protocol::Reply, state::ensure_ctx},
    shared_player_state::SharedPlayerState,
};

thread_local! {
    static BRIDGE_PLAYER_STATE: RefCell<Option<Arc<SharedPlayerState>>> = const { RefCell::new(None) };
}

pub(super) fn init_bridge_state() {
    BRIDGE_PLAYER_STATE.with(|_| {});
}

pub(crate) fn tick_and_poll_remote() {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };

        let rx_guard = worker::RX.lock();

        if let Some(ref rx) = *rx_guard {
            for msg in rx.try_iter() {
                let reply = run_cmd(state, msg.cmd);
                if let Reply::SlotAllocated(_, _, ref shared, _) = reply {
                    BRIDGE_PLAYER_STATE.with(|ps| {
                        *ps.borrow_mut() = Some(Arc::clone(shared));
                    });
                }
                let _ = msg.reply_tx.send(reply);
            }
        }
        drop(rx_guard);

        if let Some(ctx) = state.ctx_mut()
            && let Err(err) = ctx.update()
        {
            warn!("session graph update in tick failed: {err:?}");
        }
    });
}

pub(crate) fn bridge_position_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.position.load(Ordering::Relaxed))
    })
}

pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration.load(Ordering::Relaxed))
    })
}

pub(crate) fn bridge_is_playing() -> bool {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|s| s.playing.load(Ordering::Relaxed))
    })
}

pub(crate) fn warm_up_audio() {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };
        if let Err(err) = ensure_ctx(state, 0) {
            warn!("audio warm-up failed: {err}");
        }
    });
}

pub(super) fn start_stream_web_audio(
    ctx: &mut FirewheelCtx<firewheel_web_audio::WebAudioBackend>,
    sample_rate: u32,
) -> Result<(), String> {
    let config = firewheel_web_audio::WebAudioConfig {
        sample_rate: NonZeroU32::new(sample_rate),
        request_input: false,
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}
