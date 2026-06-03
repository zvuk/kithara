use std::sync::{Arc, OnceLock};

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_platform::{
    Mutex,
    sync::mpsc,
    thread::{Thread, park_timeout, sleep as thread_sleep, spawn_named},
    time::Duration,
};
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use super::{
    client::SessionDispatcher,
    state::{Cmd, CmdMsg, Reply, SessionState, StartStreamFn, run_cmd},
};
use crate::error::PlayError;

/// Native session client: pushes `CmdMsg`s into a ring buffer drained by
/// a dedicated `kithara-engine` worker thread. Latency-sensitive control
/// commands hit the worker via park/unpark wake-up.
pub(crate) struct SessionClient {
    cmd_tx: Mutex<HeapProd<CmdMsg>>,
    engine_thread: Thread,
}

impl SessionClient {
    /// Back-off sleep when the command ring buffer is full (milliseconds).
    const CMD_PUSH_BACKOFF_MS: u64 = 1;

    fn call(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.push_cmd(CmdMsg { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("session thread gone".into()))?;
        reply_rx
            .recv_sync()
            .map_err(|_| PlayError::Internal("session thread gone (reply)".into()))
    }

    #[kithara::hang_watchdog]
    fn push_cmd(&self, msg: CmdMsg) -> Result<(), PlayError> {
        let mut pending = msg;
        loop {
            hang_tick!();
            match self.cmd_tx.lock_sync().try_push(pending) {
                Ok(()) => {
                    self.engine_thread.unpark();
                    return Ok(());
                }
                Err(returned) => {
                    pending = returned;
                    thread_sleep(Duration::from_millis(Self::CMD_PUSH_BACKOFF_MS));
                }
            }
        }
    }
}

impl SessionDispatcher for SessionClient {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        self.call(cmd)
    }
}

fn engine_thread<B: AudioBackend>(mut cmd_rx: HeapCons<CmdMsg>, start_stream_fn: StartStreamFn<B>) {
    /// Fallback park timeout when no commands arrive (milliseconds).
    /// The engine thread parks and wakes instantly on command push via `unpark()`.
    /// This timeout is a safety net for spurious wakeups and periodic housekeeping.
    const ENGINE_PARK_TIMEOUT_MS: u64 = 50;

    let mut state = SessionState::<B>::new(start_stream_fn);
    loop {
        let mut processed = false;
        while let Some(msg) = cmd_rx.try_pop() {
            let CmdMsg { cmd, reply_tx } = msg;
            let reply = run_cmd(&mut state, cmd);
            let _ = reply_tx.send_sync(reply);
            processed = true;
        }
        if !processed {
            park_timeout(Duration::from_millis(ENGINE_PARK_TIMEOUT_MS));
        }
    }
}

fn spawn_session_client<B: AudioBackend + Send + 'static>(
    thread_name: &'static str,
    start_stream_fn: StartStreamFn<B>,
) -> Arc<SessionClient> {
    let (cmd_tx, cmd_rx) = HeapRb::<CmdMsg>::new(SessionState::<B>::CMD_RINGBUF_CAPACITY).split();
    let handle = spawn_named(thread_name, move || {
        engine_thread::<B>(cmd_rx, start_stream_fn);
    });
    let engine_thread = handle.thread().clone();
    Arc::new(SessionClient {
        engine_thread,
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
