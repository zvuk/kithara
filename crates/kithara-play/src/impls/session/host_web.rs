use std::{
    cell::RefCell,
    num::NonZeroU32,
    sync::{Arc, LazyLock, atomic::Ordering},
};

use firewheel::FirewheelCtx;
use kithara_platform::{Mutex, sync::mpsc};
use tracing::warn;

use super::{
    super::shared_player_state::SharedPlayerState,
    client::SessionDispatcher,
    state::{Cmd, CmdMsg, Reply, SessionState, ensure_ctx, run_cmd},
};
use crate::error::PlayError;

/// Web session client: dispatch crosses the Worker ↔ main-thread boundary
/// via a thread-local state cell on the main thread plus an mpsc bridge
/// for Workers. `local == true` means we run on the main thread and
/// access the session state directly; `local == false` means a Worker
/// proxies its commands to the main thread.
pub(crate) struct SessionClient {
    /// `true` = main thread (direct state access via thread-local).
    /// `false` = Worker (remote channel proxy).
    local: bool,
}

impl SessionClient {
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        if self.local {
            WASM_SESSION_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                let state = state
                    .as_mut()
                    .ok_or_else(|| PlayError::Internal("local session state missing".into()))?;
                Ok(run_cmd(state, cmd))
            })
        } else {
            let guard = wasm_worker_bridge::TX.lock();
            let Some(ref tx) = *guard else {
                return Err(PlayError::Internal("worker channel not initialised".into()));
            };
            let tx = tx.clone();
            drop(guard);
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CmdMsg { cmd, reply_tx })
                .map_err(|_| PlayError::Internal("session host gone".into()))?;
            reply_rx
                .recv()
                .map_err(|_| PlayError::Internal("session host gone (reply)".into()))
        }
    }
}

impl SessionDispatcher for SessionClient {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        self.call(cmd)
    }
}

thread_local! {
    static WASM_SESSION_CLIENT: RefCell<Option<Arc<SessionClient>>> = const { RefCell::new(None) };
    /// Session state lives in a thread-local so that `SessionClient` itself
    /// is `Send` (needed for the Worker architecture).
    static WASM_SESSION_STATE: RefCell<Option<SessionState<firewheel_web_audio::WebAudioBackend>>> = const { RefCell::new(None) };
    /// Shared player state captured during slot allocation (for bridge reads).
    static BRIDGE_PLAYER_STATE: RefCell<Option<Arc<SharedPlayerState>>> = const { RefCell::new(None) };
}

mod wasm_worker_bridge {
    use super::*;

    /// Sender half of the Worker → main-thread session channel.
    ///
    /// Stored in a global so that Workers can clone it from `session_client()`.
    pub(super) static TX: LazyLock<Mutex<Option<mpsc::Sender<CmdMsg>>>> =
        LazyLock::new(|| Mutex::new(None));

    /// Receiver half (polled on main thread in `tick_and_poll_remote`).
    pub(super) static RX: LazyLock<Mutex<Option<mpsc::Receiver<CmdMsg>>>> =
        LazyLock::new(|| Mutex::new(None));
}

pub(crate) fn session_client() -> Arc<SessionClient> {
    WASM_SESSION_CLIENT.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some(client) = cell.as_ref() {
            return client.clone();
        }

        let is_local = wasm_worker_bridge::TX.lock().is_none();

        if is_local {
            WASM_SESSION_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if state.is_none() {
                    *state = Some(SessionState::new(start_stream_web_audio));
                }
            });
            BRIDGE_PLAYER_STATE.with(|_| {});
        }

        let client = Arc::new(SessionClient { local: is_local });
        *cell = Some(client.clone());
        client
    })
}

/// Initialise the Worker ↔ main-thread session channel.
///
/// Must be called **once** on the main thread **before** any Worker is spawned.
pub(crate) fn init_worker_channel() {
    let (tx, rx) = mpsc::channel();
    *wasm_worker_bridge::TX.lock() = Some(tx);
    *wasm_worker_bridge::RX.lock() = Some(rx);
}

/// Poll pending session commands from Workers and run graph tick.
///
/// Called from the main thread's `requestAnimationFrame` loop.
pub(crate) fn tick_and_poll_remote() {
    WASM_SESSION_STATE.with(|state_cell| {
        let mut state_opt = state_cell.borrow_mut();
        let Some(ref mut state) = *state_opt else {
            return;
        };

        let rx_guard = wasm_worker_bridge::RX.lock();

        if let Some(ref rx) = *rx_guard {
            while let Ok(msg) = rx.try_recv() {
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

/// Read current playback position from the bridge-captured shared state.
pub(crate) fn bridge_position_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.position.load(Ordering::Relaxed))
    })
}

/// Read current media duration from the bridge-captured shared state.
pub(crate) fn bridge_duration_secs() -> f64 {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration.load(Ordering::Relaxed))
    })
}

/// Read playing state from the bridge-captured shared state.
pub(crate) fn bridge_is_playing() -> bool {
    BRIDGE_PLAYER_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|s| s.playing.load(Ordering::Relaxed))
    })
}

/// Pre-initialise the audio context and AudioWorklet eagerly.
///
/// Must be called on the main thread **after** [`session_client()`] has
/// initialised the session state. Creates the AudioContext (which starts
/// suspended) and kicks off the async AudioWorklet module load. Once the
/// module is ready, `firewheel-web-audio` registers auto-resume listeners
/// for user-gesture events (click, keydown, …). This way the very first
/// user click resumes the context — no second click required.
///
/// Passing `sample_rate = 0` makes `NonZeroU32::new(0)` return `None`,
/// letting the browser pick the default sample rate.
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

fn start_stream_web_audio(
    ctx: &mut FirewheelCtx<firewheel_web_audio::WebAudioBackend>,
    sample_rate: u32,
) -> Result<(), String> {
    let config = firewheel_web_audio::WebAudioConfig {
        sample_rate: NonZeroU32::new(sample_rate),
        request_input: false,
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}
