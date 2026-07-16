use std::{cell::RefCell, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::FirewheelCtx;
use kithara_platform::sync::{Arc, mpsc};
use tracing::warn;

use super::client::WASM_SESSION_STATE;
use crate::{
    bridge::PlaybackShared,
    session::{
        dispatch::{run_cmd, tick_session},
        protocol::{CmdMsg, Reply},
        state::ensure_ctx,
    },
};

thread_local! {
    static BRIDGE_PLAYBACK: RefCell<Option<Arc<PlaybackShared>>> = const { RefCell::new(None) };
}

pub(super) fn init_bridge_state() {
    BRIDGE_PLAYBACK.with(|_| {});
}

pub(crate) fn tick_and_poll_remote(rx: &mpsc::Receiver<CmdMsg>) {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };

        for msg in rx.try_iter() {
            let reply = run_cmd(state, msg.cmd);
            if let Reply::SlotAllocated(ref allocated) = reply {
                BRIDGE_PLAYBACK.with(|ps| {
                    *ps.borrow_mut() = Some(Arc::clone(&allocated.control.playback));
                });
            }
            let _ = msg.reply_tx.send(reply);
        }

        if let Reply::Err(error) = tick_session(state) {
            warn!(%error, "session graph update in tick failed");
        }
    });
}

pub(crate) fn bridge_position_secs() -> f64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.position.load(Ordering::Relaxed))
    })
}

pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration.load(Ordering::Relaxed))
    })
}

pub(crate) fn bridge_is_playing() -> bool {
    BRIDGE_PLAYBACK.with(|cell| {
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
