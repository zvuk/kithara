use std::sync::{Arc, OnceLock};

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_platform::{
    sync::{Mutex, mpsc},
    thread::spawn_named,
};
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use super::{
    dispatch::run_cmd,
    protocol::{Cmd, CmdMsg, Reply, SessionDispatcher, StartStreamFn},
    state::SessionState,
};
use crate::error::PlayError;

pub(crate) struct SessionClient {
    cmd_tx: Mutex<mpsc::Sender<CmdMsg>>,
}

impl SessionClient {
    /// `no_block`: sync command-reply bridge to the dedicated session thread for host/FFI dispatch.
    #[kithara::allow_block]
    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .lock()
            .send(CmdMsg { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("session thread gone".into()))?;
        let reply = reply_rx
            .recv()
            .map_err(|_| PlayError::Internal("session thread gone (reply)".into()))?;
        Ok(reply)
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
    debug!("[KITHARA-ROUTE] native session worker started");
    for CmdMsg { cmd, reply_tx } in cmd_rx.iter() {
        let reply = run_cmd(&mut state, cmd);
        if reply_tx.send(reply).is_err() {
            warn!("[KITHARA-ROUTE] native session reply receiver dropped");
        }
    }
    debug!("[KITHARA-ROUTE] native session worker stopped");
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

mod holder {
    use super::*;

    pub(super) static SESSION_CLIENT: OnceLock<Arc<SessionClient>> = OnceLock::new();
}

pub(crate) fn session_client() -> Arc<dyn SessionDispatcher> {
    holder::SESSION_CLIENT
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
    debug!(sample_rate, "[KITHARA-ROUTE] starting cpal stream");
    let config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    };
    match ctx.start_stream(config) {
        Ok(()) => {
            debug!(sample_rate, "[KITHARA-ROUTE] cpal stream started");
            Ok(())
        }
        Err(err) => {
            warn!(
                sample_rate,
                ?err,
                "[KITHARA-ROUTE] cpal stream start failed"
            );
            Err(err.to_string())
        }
    }
}
