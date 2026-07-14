use std::sync::{
    LazyLock,
    atomic::{AtomicI64, Ordering},
};

use kithara_platform::sync::{Mutex, MutexGuard, mpsc};
use kithara_play::{CmdMsg, wasm};
use wasm_bindgen::JsValue;

use crate::web::commands::WorkerCmd;

struct SessionChannel {
    tx: mpsc::Sender<CmdMsg>,
    rx: mpsc::Receiver<CmdMsg>,
}

fn current_track_id_cell() -> &'static AtomicI64 {
    static CELL: AtomicI64 = AtomicI64::new(WorkerBridge::NO_CURRENT_TRACK);
    &CELL
}

fn session_channel() -> &'static Mutex<Option<SessionChannel>> {
    static CHANNEL: LazyLock<Mutex<Option<SessionChannel>>> = LazyLock::new(|| Mutex::new(None));
    &CHANNEL
}

fn ensure_session_channel() -> mpsc::Sender<CmdMsg> {
    let mut guard = session_channel().lock();
    if let Some(channel) = guard.as_ref() {
        return channel.tx.clone();
    }

    wasm::ensure_main_session();
    let (tx, rx) = wasm::worker_session_channel();
    *guard = Some(SessionChannel { tx: tx.clone(), rx });
    tx
}

pub(crate) fn tick_and_poll() {
    let guard = session_channel().lock();
    if let Some(channel) = guard.as_ref() {
        wasm::tick_and_poll(&channel.rx);
    }
}

/// Record the worker's current track id for the main-thread read-back.
/// Called from the worker's event source on every `CurrentTrackChanged`.
pub(crate) fn set_current_track_id(id: Option<kithara_queue::TrackId>) {
    let raw = id.map_or(WorkerBridge::NO_CURRENT_TRACK, |id| {
        i64::try_from(id.as_u64()).unwrap_or(WorkerBridge::NO_CURRENT_TRACK)
    });
    current_track_id_cell().store(raw, Ordering::Relaxed);
}

/// Owns the command channel to the engine
/// [`worker`](crate::web::worker) and lazily boots it on first use. Held
/// by [`WasmInner`](crate::web::inner::WasmInner) as the wasm-side
/// counterpart of `NativeInner`'s direct `Queue` handle.
///
/// The worker itself owns the `Arc<Queue>`; this bridge only forwards
/// [`WorkerCmd`]s and (re)spawns the worker if the channel closes.
#[derive(Default)]
pub(crate) struct WorkerBridge {
    cmd_tx: Mutex<Option<mpsc::Sender<WorkerCmd>>>,
    start_lock: Mutex<()>,
}

impl WorkerBridge {
    /// Sentinel stored in [`CURRENT_TRACK_ID`] when no track is current.
    const NO_CURRENT_TRACK: i64 = -1;

    /// Id of the worker's current track, read synchronously from the
    /// shared current-track atomic the worker's event source keeps
    /// in sync. `None` when no track is current.
    pub(crate) fn current_track_id(&self) -> Option<kithara_queue::TrackId> {
        let _ = self;
        match current_track_id_cell().load(Ordering::Relaxed) {
            Self::NO_CURRENT_TRACK => None,
            raw => u64::try_from(raw).ok().map(kithara_queue::TrackId),
        }
    }

    /// Current item duration (seconds) read from the worker's audio
    /// session bridge. `0.0` when unknown.
    pub(crate) fn duration_secs(&self) -> f64 {
        let _ = self;
        wasm::bridge_duration_secs()
    }

    /// Boot the engine worker once. Idempotent: subsequent calls return
    /// early while a live channel exists.
    pub(crate) fn ensure_worker_started(&self) {
        if self.lock_cmd_tx().is_some() {
            return;
        }

        let _start_guard = self.start_lock.lock();
        if self.lock_cmd_tx().is_some() {
            return;
        }

        let session_tx = ensure_session_channel();
        // Creates the (suspended) AudioContext + loads the AudioWorklet so the
        // worker's Remote session has a Local output to hand decoded samples
        // to; firewheel-web-audio auto-resumes it on the first user gesture.
        // Without this no AudioContext exists, the session handshake never
        // completes, and the worker stalls before making a track current.
        wasm::warm_up_audio();

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = kithara_platform::thread::spawn(move || {
            crate::web::worker::worker_main(cmd_rx, session_tx);
        });
        std::mem::forget(worker);
    }

    /// Whether the worker's audio session is currently playing.
    pub(crate) fn is_playing(&self) -> bool {
        let _ = self;
        wasm::bridge_is_playing()
    }

    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock()
    }

    /// Live playback position (seconds) read from the worker's audio
    /// session bridge. `0.0` when no item is loaded.
    pub(crate) fn position_secs(&self) -> f64 {
        let _ = self;
        wasm::bridge_position_secs()
    }

    /// Forward a command to the worker, respawning the worker once if the
    /// channel has closed.
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel cannot be
    /// (re)established.
    pub(crate) fn send(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.ensure_worker_started();

        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        if tx.send(cmd.clone()).is_ok() {
            return Ok(());
        }

        *self.lock_cmd_tx() = None;
        self.ensure_worker_started();
        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        tx.send(cmd)
            .map_err(|_| JsValue::from_str("worker channel closed"))
    }
}
