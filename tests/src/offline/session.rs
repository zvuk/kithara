use kithara::{
    platform::{
        sync::{Arc, Mutex, mpsc},
        thread::{JoinHandle, spawn_named},
        time::{Duration, Instant},
    },
    play::{Cmd, PlayError, Reply, SessionDispatcher, SessionState, run_cmd},
};
use tracing::warn;

use super::backend::{OfflineBackend, OfflineConfig};

const OFFLINE_BLOCK_FRAMES: usize = 512;
const OFFLINE_PARK_MS: u64 = 10;

enum OfflineMsg {
    Cmd {
        cmd: Cmd,
        reply_tx: mpsc::Sender<Reply>,
    },
    Render {
        frames: usize,
        reply_tx: mpsc::Sender<Vec<f32>>,
    },
    Shutdown,
}

pub struct OfflineSession {
    cmd_tx: Mutex<mpsc::Sender<OfflineMsg>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl OfflineSession {
    /// Auto-render mode: the worker periodically pulls one block of
    /// audio through the graph so playback advances even when the test
    /// thread never calls [`render`](Self::render).
    #[must_use]
    pub fn new() -> Self {
        Self::spawn(true)
    }

    /// Manual mode: the worker only dispatches commands; the audio
    /// graph advances only when [`render`](Self::render) is called.
    #[must_use]
    pub fn new_manual() -> Self {
        Self::spawn(false)
    }

    /// Convenience: `Arc<dyn SessionDispatcher>` over a fresh
    /// auto-render session. Use when the test wires the dispatcher
    /// into [`kithara::play::EngineConfig::session`] and never calls
    /// [`render`](Self::render) directly.
    #[must_use]
    pub fn arc_auto() -> Arc<dyn SessionDispatcher> {
        Arc::new(Self::new())
    }

    /// Convenience: `Arc<dyn SessionDispatcher>` over a fresh
    /// manual-render session. Use when the test owns rendering via
    /// [`render`](Self::render) (rare — most callers want an
    /// `Arc<OfflineSession>` so they can keep both the dyn handle and
    /// the render API).
    #[must_use]
    pub fn arc_manual() -> Arc<dyn SessionDispatcher> {
        Arc::new(Self::new_manual())
    }

    fn spawn(auto_render: bool) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<OfflineMsg>();
        let handle = spawn_named("kithara-engine-offline-instance", move || {
            offline_session_thread(&cmd_rx, auto_render);
        });
        Self {
            cmd_tx: Mutex::new(cmd_tx),
            worker: Mutex::new(Some(handle)),
        }
    }

    /// Synchronously drive one render iteration. Returns
    /// stereo-interleaved samples, or an empty `Vec` if the firewheel
    /// context has not been initialised yet (no player started).
    /// `no_block`: sync command-reply bridge to the dedicated offline render thread; flash coordinates the bridged wait.
    #[kithara::allow_block]
    pub fn render(&self, frames: usize) -> Vec<f32> {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .cmd_tx
            .lock()
            .send(OfflineMsg::Render { frames, reply_tx })
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }
}

impl Default for OfflineSession {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OfflineSession {
    fn drop(&mut self) {
        // `Shutdown` lands on the engine-aware command channel; `run_auto`/
        // `run_manual` block on its arrival event (`recv`/`recv_timeout`),
        // so the render thread wakes the instant this send signals the condvar —
        // no `unpark`, and no dependence on a virtual-clock advance the joining
        // thread below would otherwise pin.
        let _ = self.cmd_tx.lock().send(OfflineMsg::Shutdown);
        if let Some(handle) = self.worker.lock().take() {
            let _ = handle.join();
        }
    }
}

impl SessionDispatcher for OfflineSession {
    /// `no_block`: sync command-reply bridge to the dedicated offline render thread; flash coordinates the bridged wait.
    #[kithara::allow_block]
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .lock()
            .send(OfflineMsg::Cmd { cmd, reply_tx })
            .map_err(|_| PlayError::Internal("offline session worker gone".into()))?;
        reply_rx
            .recv()
            .map_err(|_| PlayError::Internal("offline session worker gone (reply)".into()))
    }
}

fn start_stream_offline(
    ctx: &mut firewheel::FirewheelCtx<OfflineBackend>,
    sample_rate: u32,
) -> Result<(), String> {
    let config = OfflineConfig {
        sample_rate,
        block_frames: u32::try_from(OFFLINE_BLOCK_FRAMES).unwrap_or(u32::MAX),
    };
    ctx.start_stream(config).map_err(|err| err.to_string())
}

fn offline_session_thread(cmd_rx: &mpsc::Receiver<OfflineMsg>, auto_render: bool) {
    let mut state = SessionState::<OfflineBackend>::new(start_stream_offline);
    if auto_render {
        run_auto(&mut state, cmd_rx);
    } else {
        run_manual(&mut state, cmd_rx);
    }
}

fn run_manual(state: &mut SessionState<OfflineBackend>, cmd_rx: &mpsc::Receiver<OfflineMsg>) {
    for msg in cmd_rx.iter() {
        match msg {
            OfflineMsg::Cmd { cmd, reply_tx } => {
                let reply = run_cmd(state, cmd);
                let _ = reply_tx.send(reply);
            }
            OfflineMsg::Render { frames, reply_tx } => {
                let block = render_block(state, frames);
                let _ = reply_tx.send(block);
            }
            OfflineMsg::Shutdown => break,
        }
    }
}

fn run_auto(state: &mut SessionState<OfflineBackend>, cmd_rx: &mpsc::Receiver<OfflineMsg>) {
    loop {
        // Block on the next command, but no longer than one render budget: a
        // command (or `Shutdown`) wakes us at once through the engine-aware
        // channel, while the timeout drives the periodic auto-render so playback
        // advances even when the test thread never sends anything. There is no
        // `park_timeout` to lose a cross-thread wake against.
        let deadline = Instant::now() + Duration::from_millis(OFFLINE_PARK_MS);
        match cmd_rx.recv_timeout(deadline) {
            Ok(OfflineMsg::Cmd { cmd, reply_tx }) => {
                let reply = run_cmd(state, cmd);
                let _ = reply_tx.send(reply);
            }
            Ok(OfflineMsg::Render { frames, reply_tx }) => {
                let block = render_block(state, frames);
                let _ = reply_tx.send(block);
            }
            Ok(OfflineMsg::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = render_block(state, OFFLINE_BLOCK_FRAMES);
            }
        }
    }
}

fn render_block(state: &mut SessionState<OfflineBackend>, frames: usize) -> Vec<f32> {
    if let Reply::Err(error) = run_cmd(state, Cmd::Tick) {
        warn!(%error, "offline session graph update failed");
    }
    let Some(ctx) = state.ctx_mut() else {
        return Vec::new();
    };
    ctx.active_backend_mut()
        .map_or_else(Vec::new, |backend| backend.render(frames))
}
