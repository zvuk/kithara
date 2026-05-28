use wasm_bindgen::JsValue;

use crate::web::{bridge::WorkerBridge, commands::WorkerCmd};

/// Wasm implementation of the FFI player engine, parallel to
/// [`NativeInner`](crate::native::inner::NativeInner).
///
/// The worker owns the `Arc<Queue>`; `WasmInner` owns the command channel
/// into it. This skeleton wires the playback-control surface as typed
/// methods and exposes [`Self::dispatch`] for the structural queue-edit
/// commands (append / insert / remove / replace / select / remove-all),
/// which the [`queue`](crate::web::queue) wasm-bindgen surface builds with
/// the caller-allocated [`TrackId`](kithara_queue::TrackId) +
/// `request_id`. It is built alongside the legacy
/// [`Player`](crate::web::player) singleton and is **not** yet connected
/// to the [`AudioPlayer`](crate::player) facade — Wave 4 does the wiring.
///
/// Methods that need round-trip state read-back from the worker (snapshot,
/// position, item list), the observer event bridge, or DRM key plumbing
/// are intentionally absent here: they require Wave 4/5 channels that do
/// not exist yet, and stubbing them would lie about the engine state. See
/// the Wave 3 handoff for the deferred list.
#[derive(Default)]
pub(crate) struct WasmInner {
    bridge: WorkerBridge,
}

impl WasmInner {
    /// Boot the engine worker eagerly so the first command does not pay
    /// the spawn cost.
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the worker spawn or session bootstrap
    /// fails.
    pub(crate) fn start(&self) -> Result<(), JsValue> {
        self.bridge.ensure_worker_started()
    }

    /// Forward a pre-built [`WorkerCmd`] to the engine worker. Used by the
    /// structural queue-edit surface, where the command (with its
    /// caller-allocated [`TrackId`](kithara_queue::TrackId) and reply
    /// `request_id`) is assembled in [`crate::web::queue`].
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn dispatch(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.bridge.send(cmd)
    }

    /// Start playback. Mirrors
    /// [`NativeInner::play`](crate::native::inner::NativeInner::play).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn play(&self) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::Play)
    }

    /// Pause playback. Mirrors
    /// [`NativeInner::pause`](crate::native::inner::NativeInner::pause).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn pause(&self) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::Pause)
    }

    /// Pause and clear the queue. Mirrors
    /// [`NativeInner::stop`](crate::native::inner::NativeInner::stop).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn stop(&self) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::Stop)
    }

    /// Seek the current track. `seconds` is converted to the worker's
    /// millisecond `Seek` command.
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn seek(&self, seconds: f64) -> Result<(), JsValue> {
        /// Milliseconds per second.
        const MS_PER_SECOND: f64 = 1000.0;
        self.bridge
            .send(WorkerCmd::Seek(seconds.max(0.0) * MS_PER_SECOND))
    }

    /// Set the output volume (0.0..=1.0). Mirrors
    /// [`NativeInner::set_volume`](crate::native::inner::NativeInner::set_volume).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn set_volume(&self, volume: f32) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::SetVolume(volume))
    }

    /// Set the crossfade duration in seconds. Mirrors
    /// [`NativeInner::set_crossfade_duration`](crate::native::inner::NativeInner::set_crossfade_duration).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn set_crossfade_duration(&self, seconds: f32) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::SetCrossfade(seconds))
    }

    /// Set the gain for an EQ band. Mirrors
    /// [`NativeInner::set_eq_gain`](crate::native::inner::NativeInner::set_eq_gain).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::SetEqGain { band, gain_db })
    }

    /// Reset every EQ band to 0 dB. Mirrors
    /// [`NativeInner::reset_eq`](crate::native::inner::NativeInner::reset_eq).
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel is unavailable.
    pub(crate) fn reset_eq(&self) -> Result<(), JsValue> {
        self.bridge.send(WorkerCmd::ResetEq)
    }
}
