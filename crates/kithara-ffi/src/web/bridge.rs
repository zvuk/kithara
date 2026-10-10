use std::sync::{
    LazyLock, OnceLock,
    atomic::{AtomicI64, Ordering},
};

use kithara::{
    host::{HostConfig, wasm},
    platform::{
        sync::{Mutex, MutexGuard, mpsc},
        thread,
    },
    play::wasm as play_wasm,
    queue::TrackId,
};
use wasm_bindgen::JsValue;

use crate::{
    pools::{FfiHost, FfiPools, FfiQueueControl, Pools, build as build_pools},
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

fn host_channel() -> &'static Mutex<Option<HostChannel>> {
    static CHANNEL: LazyLock<Mutex<Option<HostChannel>>> = LazyLock::new(|| Mutex::new(None));
    &CHANNEL
}

fn ensure_host_channel() -> Result<(wasm::HostSender<FfiPools>, Pools), JsValue> {
    let mut guard = host_channel().lock();
    if let Some(channel) = guard.as_ref() {
        return Ok((channel.sender.clone(), channel.pools.clone()));
    }

    let pools = build_pools()
        .map_err(|error| JsValue::from_str(&format!("pool construction failed: {error}")))?;
    let host = FfiHost::new(HostConfig::builder().build())
        .map_err(|error| JsValue::from_str(&format!("host construction failed: {error}")))?;
    let (sender, receiver) = wasm::worker_host_channel(&host)
        .map_err(|error| JsValue::from_str(&format!("host channel failed: {error}")))?;
    play_wasm::spawn_webcodecs_probe(pools.clone());
    *guard = Some(HostChannel {
        receiver,
        _host: host,
        pools: pools.clone(),
        sender: sender.clone(),
    });
    Ok((sender, pools))
}

pub(crate) fn initialize() -> Result<(), JsValue> {
    ensure_host_channel().map(drop)
}

pub(crate) fn tick_and_poll() {
    let guard = host_channel().lock();
    if let Some(channel) = guard.as_ref() {
        wasm::tick_and_poll(&channel.receiver);
    }
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
#[derive(Default)]
pub(crate) struct WorkerBridge {
    cmd_tx: Mutex<Option<mpsc::Sender<WorkerCmd>>>,
    start_lock: Mutex<()>,
    queue: OnceLock<FfiQueueControl>,
    queue_rx: Mutex<Option<mpsc::Receiver<FfiQueueControl>>>,
}

impl WorkerBridge {
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

    fn queue(&self) -> Option<&FfiQueueControl> {
        if let Some(queue) = self.queue.get() {
            return Some(queue);
        }
        let received = self.queue_rx.lock().as_ref()?.try_recv().ok()?;
        Some(self.queue.get_or_init(|| received))
    }

    /// Current item duration (seconds) read from the deck's published
    /// snapshot. `0.0` when unknown.
    pub(crate) fn duration_secs(&self) -> f64 {
        self.queue()
            .and_then(FfiQueueControl::duration_seconds)
            .unwrap_or(0.0)
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

        let Ok((host_sender, pools)) = ensure_host_channel() else {
            return;
        };

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (queue_tx, queue_rx) = mpsc::channel();
        *self.queue_rx.lock() = Some(queue_rx);
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = thread::spawn(move || {
            crate::web::worker::worker_main(cmd_rx, host_sender, pools, queue_tx);
        });
        std::mem::forget(worker);
    }

    delegate::delegate! {
        to self {
            /// Whether the deck's published snapshot is currently playing.
            #[expr($.is_some_and(FfiQueueControl::is_playing))]
            #[call(queue)]
            pub(crate) fn is_playing(&self) -> bool;
            /// Audio-thread process calls served so far. Monotonic, so a caller
            /// samples twice and reads the delta.
            #[expr($.map_or(0, FfiQueueControl::mixer_blocks))]
            #[call(queue)]
            pub(crate) fn process_calls(&self) -> u64;
            /// Underruns the audio thread has recorded so far.
            #[expr($.map_or(0, |queue| queue.mixer_metrics().underruns()))]
            #[call(queue)]
            pub(crate) fn underruns(&self) -> u64;
        }
    }

    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock()
    }

    /// Live playback position (seconds) read from the deck's published
    /// snapshot. `0.0` when no item is loaded.
    pub(crate) fn position_secs(&self) -> f64 {
        self.queue()
            .and_then(FfiQueueControl::position_seconds)
            .unwrap_or(0.0)
    }

    /// Forward a command to the worker.
    ///
    /// # Errors
    /// Returns a [`JsValue`] error if the command channel cannot be
    /// established or the canonical worker has exited. A closed worker is not
    /// respawned because the main thread cannot prove that its old Host member
    /// was detached before creating a replacement.
    pub(crate) fn send(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
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
