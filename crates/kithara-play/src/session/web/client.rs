use std::{cell::RefCell, sync::Arc};

use kithara_platform::sync::mpsc;

use super::bridge::{init_bridge_state, start_stream_web_audio};
use crate::{
    error::PlayError,
    session::{
        dispatch::run_cmd,
        protocol::{Cmd, CmdMsg, Reply, SessionDispatcher, SessionHandle},
        state::SessionState,
    },
};

enum SessionHost {
    Local,
    Remote { tx: mpsc::Sender<CmdMsg> },
}

pub(crate) struct SessionClient {
    host: SessionHost,
}

impl SessionClient {
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        match &self.host {
            SessionHost::Local => WASM_SESSION_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                let state = state
                    .as_mut()
                    .ok_or_else(|| PlayError::Internal("local session state missing".into()))?;
                Ok(run_cmd(state, cmd))
            }),
            SessionHost::Remote { tx } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                tx.send(CmdMsg { cmd, reply_tx })
                    .map_err(|_| PlayError::Internal("session host gone".into()))?;
                reply_rx
                    .recv()
                    .map_err(|_| PlayError::Internal("session host gone (reply)".into()))
            }
        }
    }
}

impl SessionDispatcher for SessionClient {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        self.call(cmd)
    }
}

thread_local! {
    pub(super) static WASM_SESSION_STATE: RefCell<Option<SessionState<firewheel_web_audio::WebAudioBackend>>> = const { RefCell::new(None) };
}

pub(crate) fn local_session() -> SessionHandle {
    WASM_SESSION_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.is_none() {
            *state = Some(SessionState::new(start_stream_web_audio));
        }
    });
    init_bridge_state();
    SessionHandle::new(Arc::new(SessionClient {
        host: SessionHost::Local,
    }))
}

pub(crate) fn remote_session(tx: mpsc::Sender<CmdMsg>) -> SessionHandle {
    SessionHandle::new(Arc::new(SessionClient {
        host: SessionHost::Remote { tx },
    }))
}

pub(crate) fn worker_channel() -> (mpsc::Sender<CmdMsg>, mpsc::Receiver<CmdMsg>) {
    mpsc::channel()
}
