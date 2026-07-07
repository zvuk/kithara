use std::{
    cell::RefCell,
    sync::{Arc, LazyLock},
};

use kithara_platform::sync::{Mutex, mpsc};

use super::bridge::{init_bridge_state, start_stream_web_audio};
use crate::{
    error::PlayError,
    impls::session::{
        dispatch::run_cmd,
        protocol::{Cmd, CmdMsg, Reply, SessionDispatcher},
        state::SessionState,
    },
};

pub(crate) struct SessionClient {
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
            let guard = worker::TX.lock();
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
    pub(super) static WASM_SESSION_STATE: RefCell<Option<SessionState<firewheel_web_audio::WebAudioBackend>>> = const { RefCell::new(None) };
}

pub(super) mod worker {
    use super::*;

    pub(crate) static TX: LazyLock<Mutex<Option<mpsc::Sender<CmdMsg>>>> =
        LazyLock::new(|| Mutex::new(None));
    pub(crate) static RX: LazyLock<Mutex<Option<mpsc::Receiver<CmdMsg>>>> =
        LazyLock::new(|| Mutex::new(None));
}

pub(crate) fn session_client() -> Arc<dyn SessionDispatcher> {
    WASM_SESSION_CLIENT.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some(client) = cell.as_ref() {
            return client.clone();
        }

        let is_local = worker::TX.lock().is_none();

        if is_local {
            WASM_SESSION_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if state.is_none() {
                    *state = Some(SessionState::new(start_stream_web_audio));
                }
            });
            init_bridge_state();
        }

        let client = Arc::new(SessionClient { local: is_local });
        *cell = Some(client.clone());
        client
    })
}

pub(crate) fn init_worker_channel() {
    let (tx, rx) = mpsc::channel();
    *worker::TX.lock() = Some(tx);
    *worker::RX.lock() = Some(rx);
}
