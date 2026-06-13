use std::sync::atomic::{AtomicI64, Ordering};

use kithara_platform::sync::{Mutex, MutexGuard, mpsc};
use kithara_play::wasm_support;
use wasm_bindgen::JsValue;

use crate::web::commands::WorkerCmd;

/// Worker → main mirror of the current track id, written by the worker's
/// event source on `CurrentTrackChanged` and read synchronously on the
/// main thread by [`WorkerBridge::current_track_id`]. Lives in shared wasm
/// linear memory (`SharedArrayBuffer`), so the atomic is visible across the
/// worker boundary without a round-trip. `-1` is the "no current track"
/// sentinel ([`TrackId`](kithara_queue::TrackId) values are small
/// monotonic `u64`s that never reach the `i64` sign bit in practice).
static CURRENT_TRACK_ID: AtomicI64 = AtomicI64::new(WorkerBridge::NO_CURRENT_TRACK);

/// Record the worker's current track id for the main-thread read-back.
/// Called from the worker's event source on every `CurrentTrackChanged`.
pub(crate) fn set_current_track_id(id: Option<kithara_queue::TrackId>) {
    let raw = id.map_or(WorkerBridge::NO_CURRENT_TRACK, |id| {
        i64::try_from(id.as_u64()).unwrap_or(WorkerBridge::NO_CURRENT_TRACK)
    });
    CURRENT_TRACK_ID.store(raw, Ordering::Relaxed);
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

    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock()
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

    /// Id of the worker's current track, read synchronously from the
    /// shared [`CURRENT_TRACK_ID`] atomic the worker's event source keeps
    /// in sync. `None` when no track is current.
    pub(crate) fn current_track_id(&self) -> Option<kithara_queue::TrackId> {
        let _ = self;
        match CURRENT_TRACK_ID.load(Ordering::Relaxed) {
            Self::NO_CURRENT_TRACK => None,
            raw => u64::try_from(raw).ok().map(kithara_queue::TrackId),
        }
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

        wasm_support::ensure_main_session();
        wasm_support::init_worker_session();
        // Creates the (suspended) AudioContext + loads the AudioWorklet so the
        // worker's Remote session has a Local output to hand decoded samples
        // to; firewheel-web-audio auto-resumes it on the first user gesture.
        // Without this no AudioContext exists, the session handshake never
        // completes, and the worker stalls before making a track current.
        wasm_support::warm_up_audio();

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = kithara_platform::thread::spawn(move || {
            crate::web::worker::worker_main(cmd_rx);
        });
        std::mem::forget(worker);
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
