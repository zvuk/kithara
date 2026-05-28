use kithara_platform::sync::{Mutex, MutexGuard, mpsc};
use kithara_play::wasm_support;
use wasm_bindgen::JsValue;

use crate::web::commands::WorkerCmd;

/// Owns the command channel to the engine [`Worker`](crate::web::worker)
/// and lazily boots it on first use. Mirrors the worker-bootstrap half of
/// the legacy [`Player`](crate::web::player) singleton, but as an
/// instance so [`WasmInner`](crate::web::inner::WasmInner) can hold one.
///
/// The worker itself owns the `Arc<Queue>`; this bridge only forwards
/// [`WorkerCmd`]s and (re)spawns the worker if the channel closes.
#[derive(Default)]
pub(crate) struct WorkerBridge {
    cmd_tx: Mutex<Option<mpsc::Sender<WorkerCmd>>>,
    start_lock: Mutex<()>,
}

impl WorkerBridge {
    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock_sync()
    }

    /// Live playback position (seconds) read from the worker's audio
    /// session bridge. `0.0` when no item is loaded.
    pub(crate) fn position_secs(&self) -> f64 {
        let _ = self;
        wasm_support::bridge_position_secs()
    }

    /// Current item duration (seconds) read from the worker's audio
    /// session bridge. `0.0` when unknown.
    pub(crate) fn duration_secs(&self) -> f64 {
        let _ = self;
        wasm_support::bridge_duration_secs()
    }

    /// Whether the worker's audio session is currently playing.
    pub(crate) fn is_playing(&self) -> bool {
        let _ = self;
        wasm_support::bridge_is_playing()
    }

    /// Id of the worker's current track, if the worker has a synchronous
    /// read-back for it. Not yet wired (Wave 5): the worker owns the
    /// current-track cursor and does not mirror it to the main thread, so
    /// this returns `None` and the facade reports "no current item".
    pub(crate) fn current_track_id(&self) -> Option<kithara_queue::TrackId> {
        let _ = self;
        None
    }

    /// Boot the engine worker once. Idempotent: subsequent calls return
    /// early while a live channel exists. Mirrors
    /// [`Player::ensure_worker_started`](crate::web::player).
    pub(crate) fn ensure_worker_started(&self) {
        if self.lock_cmd_tx().is_some() {
            return;
        }

        let _start_guard = self.start_lock.lock_sync();
        if self.lock_cmd_tx().is_some() {
            return;
        }

        wasm_support::ensure_main_session();
        wasm_support::init_worker_session();
        crate::web::js::init_event_reader();

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = kithara_platform::spawn(move || {
            crate::web::worker::worker_main(cmd_rx);
        });
        std::mem::forget(worker);
    }

    /// Forward a command to the worker, respawning the worker once if the
    /// channel has closed. Mirrors
    /// [`Player::send_cmd`](crate::web::player).
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
        if tx.send_sync(cmd.clone()).is_ok() {
            return Ok(());
        }

        *self.lock_cmd_tx() = None;
        self.ensure_worker_started();
        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        tx.send_sync(cmd)
            .map_err(|_| JsValue::from_str("worker channel closed"))
    }
}
