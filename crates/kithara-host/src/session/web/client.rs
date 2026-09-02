use std::cell::Cell;

use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::HasPool;
use kithara_platform::sync::{Arc, Mutex, mpsc};
use kithara_play::{GroupState, StreamShape, player::PlayerMember};

use super::bridge::{init_bridge_state, reset_bridge_state, start_stream_web_audio};
use crate::{
    error::PlayError,
    session::{
        dispatch::run_host_cmd,
        protocol::{
            Cmd, HostCmd, HostCmdMsg, HostDispatchError, HostDispatcher, HostReply, Reply,
            SessionDispatcher,
        },
        state::{RootView, SessionState},
    },
};

pub(crate) type WebSessionState<S> =
    Arc<Mutex<Option<SessionState<firewheel_web_audio::WebAudioBackend, S>>>>;

enum SessionHost<S> {
    Local { state: WebSessionState<S> },
    Remote { tx: mpsc::Sender<HostCmdMsg<S>> },
}

pub(crate) struct SessionClient<S> {
    host: SessionHost<S>,
}

impl<S> SessionClient<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn call(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        match &self.host {
            SessionHost::Local { state } => {
                if matches!(&cmd, HostCmd::Shutdown) {
                    drop(state.lock().take());
                    WASM_SESSION_ACTIVE.with(|active| active.set(false));
                    reset_bridge_state();
                    return Ok(HostReply::Ok);
                }
                let mut state = state.lock();
                match state.as_mut() {
                    Some(state) => Ok(run_host_cmd(state, cmd)),
                    None => Err(HostDispatchError::before_send(
                        PlayError::Internal("local session state missing".into()),
                        cmd,
                    )),
                }
            }
            SessionHost::Remote { tx } => {
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Err(error) = tx.send(HostCmdMsg { cmd, reply_tx }) {
                    return Err(HostDispatchError::before_send(
                        PlayError::SessionGone {
                            reason: "session host stopped accepting commands",
                        },
                        error.0.cmd,
                    ));
                }
                reply_rx.recv().map_err(|_| {
                    HostDispatchError::after_send(PlayError::SessionGone {
                        reason: "session host dropped the reply channel",
                    })
                })
            }
        }
    }
}

impl<S> SessionDispatcher<S> for SessionClient<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        match self.call(HostCmd::Play(cmd)).map_err(PlayError::from)? {
            HostReply::Play(reply) => Ok(reply),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for player session command".into(),
            )),
        }
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

impl<S> HostDispatcher<S> for SessionClient<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn exec_host(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        self.call(cmd)
    }
}

thread_local! {
    static WASM_SESSION_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn spawn<S: HasPool<f32> + Send + Sync + 'static>(
    root: GroupState<PlayerMember>,
    root_view: RootView,
    requested_shape: StreamShape,
) -> Result<(Arc<dyn HostDispatcher<S>>, WebSessionState<S>), PlayError> {
    WASM_SESSION_ACTIVE.with(|active| {
        if active.replace(true) {
            return Err(PlayError::SessionAlreadyActive);
        }
        Ok(())
    })?;
    let state = Arc::new(Mutex::new(Some(SessionState::new(
        root,
        root_view,
        requested_shape,
        start_stream_web_audio,
    ))));
    init_bridge_state();
    let client = Arc::new(SessionClient {
        host: SessionHost::Local {
            state: Arc::clone(&state),
        },
    });
    Ok((client, state))
}

pub(crate) fn remote<S: HasPool<f32> + Send + Sync + 'static>(
    tx: mpsc::Sender<HostCmdMsg<S>>,
) -> Arc<dyn HostDispatcher<S>> {
    let client = Arc::new(SessionClient {
        host: SessionHost::Remote { tx },
    });
    client
}

pub(crate) fn worker_channel<S>() -> (mpsc::Sender<HostCmdMsg<S>>, mpsc::Receiver<HostCmdMsg<S>>) {
    mpsc::channel()
}
