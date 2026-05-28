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

    /// Boot the engine worker once. Idempotent: subsequent calls return
    /// early while a live channel exists. Mirrors
    /// [`Player::ensure_worker_started`](crate::web::player).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the worker spawn or session bootstrap
    /// fails.
    pub(crate) fn ensure_worker_started(&self) -> Result<(), JsValue> {
        if self.lock_cmd_tx().is_some() {
            return Ok(());
        }

        let _start_guard = self.start_lock.lock_sync();
        if self.lock_cmd_tx().is_some() {
            return Ok(());
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
        Ok(())
    }

    /// Forward a command to the worker, respawning the worker once if the
    /// channel has closed. Mirrors
    /// [`Player::send_cmd`](crate::web::player).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel cannot be
    /// (re)established.
    pub(crate) fn send(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.ensure_worker_started()?;

        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        if tx.send_sync(cmd.clone()).is_ok() {
            return Ok(());
        }

        *self.lock_cmd_tx() = None;
        self.ensure_worker_started()?;
        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        tx.send_sync(cmd)
            .map_err(|_| JsValue::from_str("worker channel closed"))
    }
}
