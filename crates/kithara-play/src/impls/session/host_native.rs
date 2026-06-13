use std::sync::{Arc, OnceLock};

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_platform::{Mutex, sync::mpsc, thread::spawn_named};

use super::{
    client::SessionDispatcher,
    state::{Cmd, CmdMsg, Reply, SessionState, StartStreamFn, run_cmd},
};
use crate::error::PlayError;

/// Native session client: sends `CmdMsg`s over an engine-aware channel drained
/// by a dedicated `kithara-engine` worker thread. The worker blocks on the
/// command-arrival EVENT (`recv`), not a `park_timeout` budget, so a
/// latency-sensitive control command wakes it the instant it lands — under both
/// the real and the virtual clock, with no cross-thread park/unpark to lose.
pub(crate) struct SessionClient {
    cmd_tx: Mutex<mpsc::Sender<CmdMsg>>,
}

impl SessionClient {
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .lock()
            .send(CmdMsg { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("session thread gone".into()))?;
        reply_rx
            .recv()
            .map_err(|_| PlayError::Internal("session thread gone (reply)".into()))
    }
}

impl SessionDispatcher for SessionClient {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        self.call(cmd)
    }
}

fn engine_thread<B: AudioBackend>(
    cmd_rx: &mpsc::Receiver<CmdMsg>,
    start_stream_fn: StartStreamFn<B>,
) {
    let mut state = SessionState::<B>::new(start_stream_fn);
    // Block on the command-arrival event. `recv` returns `Err` only once
    // every sender has been dropped, which is the worker's exit signal.
    for CmdMsg { cmd, reply_tx } in cmd_rx.iter() {
        let reply = run_cmd(&mut state, cmd);
        let _ = reply_tx.send(reply);
    }
}

fn spawn_session_client<B: AudioBackend + Send + 'static>(
    thread_name: &'static str,
    start_stream_fn: StartStreamFn<B>,
) -> Arc<SessionClient> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<CmdMsg>();
    spawn_named(thread_name, move || {
        engine_thread::<B>(&cmd_rx, start_stream_fn);
    });
    Arc::new(SessionClient {
        cmd_tx: Mutex::new(cmd_tx),
    })
}

mod session_holder {
    use super::*;

    /// Singleton session client shared across the process. First caller
    /// wins — either `session_client()` (production, spawns cpal) or
    /// integration-test offline init.
    pub(super) static SESSION_CLIENT: OnceLock<Arc<SessionClient>> = OnceLock::new();
}

pub(crate) fn session_client() -> Arc<SessionClient> {
    session_holder::SESSION_CLIENT
        .get_or_init(|| {
            spawn_session_client::<firewheel::cpal::CpalBackend>(
                "kithara-engine",
                start_stream_cpal,
            )
        })
        .clone()
}

fn start_stream_cpal(
    ctx: &mut FirewheelCtx<firewheel::cpal::CpalBackend>,
    sample_rate: u32,
) -> Result<(), String> {
    let config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}
