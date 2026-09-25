use std::sync::{
    LazyLock,
    atomic::{AtomicI64, Ordering},
};

use kithara::{
    host::wasm,
    platform::{
        sync::{Mutex, MutexGuard, mpsc},
        thread,
    },
    play::wasm as play_wasm,
    queue::TrackId,
};
use wasm_bindgen::JsValue;

use crate::{
    FfiHostConfig, FfiQueueSettings,
    core::host::lifecycle::Lifecycle,
    pools::{FfiHost, FfiPools, Pools, build as build_pools},
    web::commands::WorkerCmd,
};

struct HostChannel {
    _host: FfiHost,
    receiver: wasm::HostReceiver<FfiPools>,
    sender: wasm::HostSender<FfiPools>,
    pools: Pools,
}

fn current_track_id_cell() -> &'static AtomicI64 {
    static CELL: AtomicI64 = AtomicI64::new(WorkerBridge::NO_CURRENT_TRACK);
    &CELL
}

fn host_channel() -> &'static Lifecycle<HostChannel> {
    static CHANNEL: LazyLock<Lifecycle<HostChannel>> = LazyLock::new(Lifecycle::default);
    &CHANNEL
}

fn ready_host_channel() -> Result<(wasm::HostSender<FfiPools>, Pools), JsValue> {
    host_channel()
        .with_ready(|channel| (channel.sender.clone(), channel.pools.clone()))
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

pub(crate) fn require_initialized() -> Result<(), JsValue> {
    ready_host_channel().map(drop)
}

pub(crate) fn require_initialized_domain() -> Result<(), crate::types::FfiError> {
    host_channel().with_ready(|_| ())
}

#[wasm_bindgen::prelude::wasm_bindgen(js_name = initializeHost)]
pub fn initialize_host(config: FfiHostConfig) -> Result<(), JsValue> {
    initialize_host_domain(config).map_err(|error| JsValue::from_str(&error.to_string()))
}

pub(crate) fn initialize_host_domain(config: FfiHostConfig) -> Result<(), crate::types::FfiError> {
    host_channel().initialize(|| {
        let host_config = config.into_domain()?;
        let pools = build_pools().map_err(|error| crate::types::FfiError::Internal {
            description: format!("pool construction failed: {error}"),
        })?;
        let host = FfiHost::new(host_config)?;
        let (sender, receiver) = wasm::worker_host_channel(&host)?;
        play_wasm::spawn_webcodecs_probe(pools.clone());
        wasm::warm_up_audio(&host)?;
        Ok(HostChannel {
            receiver,
            _host: host,
            pools,
            sender,
        })
    })
}

pub(crate) fn tick_and_poll() {
    let _ = host_channel().with_ready(|channel| {
        wasm::tick_and_poll(&channel.receiver);
    });
}

/// Record the worker's current track id for the main-thread read-back.
/// Called from the worker's event source on every `CurrentTrackChanged`.
pub(crate) fn set_current_track_id(id: Option<TrackId>) {
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
/// The worker itself owns the canonical Host member; this bridge only forwards
/// [`WorkerCmd`]s and boots the worker once.
pub(crate) struct WorkerBridge {
    cmd_tx: Mutex<Option<mpsc::Sender<WorkerCmd>>>,
    start_lock: Mutex<()>,
    queue_settings: FfiQueueSettings,
}

impl WorkerBridge {
    pub(crate) fn new(queue_settings: FfiQueueSettings) -> Self {
        Self {
            cmd_tx: Mutex::default(),
            start_lock: Mutex::default(),
            queue_settings,
        }
    }
    /// Sentinel stored in [`CURRENT_TRACK_ID`] when no track is current.
    const NO_CURRENT_TRACK: i64 = -1;

    /// Id of the worker's current track, read synchronously from the
    /// shared current-track atomic the worker's event source keeps
    /// in sync. `None` when no track is current.
    pub(crate) fn current_track_id(&self) -> Option<TrackId> {
        let _ = self;
        match current_track_id_cell().load(Ordering::Relaxed) {
            Self::NO_CURRENT_TRACK => None,
            raw => u64::try_from(raw).ok().map(TrackId),
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
    pub(crate) fn ensure_worker_started(&self) -> Result<(), JsValue> {
        if self.lock_cmd_tx().is_some() {
            return Ok(());
        }

        let _start_guard = self.start_lock.lock();
        if self.lock_cmd_tx().is_some() {
            return Ok(());
        }

        let (host_sender, pools) = ready_host_channel()?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let queue_settings = self.queue_settings;
        let worker = thread::spawn(move || {
            crate::web::worker::worker_main(cmd_rx, host_sender, pools, queue_settings);
        });
        std::mem::forget(worker);
        Ok(())
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

    /// Audio-thread process calls served so far. Monotonic, so a caller
    /// samples twice and reads the delta.
    pub(crate) fn process_calls(&self) -> u64 {
        let _ = self;
        wasm::bridge_process_calls()
    }

    /// Forward a command to the worker.
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel cannot be
    /// established or the canonical worker has exited. A closed worker is not
    /// respawned because the main thread cannot prove that its old Host member
    /// was detached before creating a replacement.
    pub(crate) fn send(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.ensure_worker_started()?;

        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        tx.send(cmd)
            .map_err(|_| JsValue::from_str("worker channel closed"))
    }

    /// Underruns the audio thread has recorded so far.
    pub(crate) fn underruns(&self) -> u64 {
        let _ = self;
        wasm::bridge_underruns()
    }
}
