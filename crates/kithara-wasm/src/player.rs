//! Worker-based WASM player.
//!
//! [`WasmPlayer`] is a `#[wasm_bindgen]` wrapper that forwards player commands
//! to the engine Worker and reads status from shared atomics. It owns the
//! firewheel session host (via [`kithara_play::wasm_support`]) and drives graph
//! updates in `tick()`.
//!
//! Getter values (volume, crossfade, EQ, ducking) are cached on the main thread
//! so that reads never require a Worker round-trip.

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use kithara_platform::Mutex;
use kithara_play::wasm_support;
use tracing::info;
use wasm_bindgen::prelude::*;

use crate::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

const CROSSFADE_SECONDS: f32 = 5.0;
const EQ_BANDS: usize = 10;

fn js_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

/// Cached player settings — main-thread mirror of Worker state.
/// Written by setters, read by getters. No Worker round-trip.
struct CachedState {
    volume: Cell<f32>,
    crossfade_secs: Cell<f32>,
    eq_gains: RefCell<Vec<f32>>,
    ducking: Cell<u32>,
    eq_bands: u32,
}

impl CachedState {
    fn new() -> Self {
        Self {
            volume: Cell::new(1.0),
            crossfade_secs: Cell::new(CROSSFADE_SECONDS),
            eq_gains: RefCell::new(vec![0.0; EQ_BANDS]),
            ducking: Cell::new(0),
            eq_bands: EQ_BANDS as u32,
        }
    }
}

#[wasm_bindgen]
pub struct WasmPlayer {
    cmd_tx: kithara_platform::sync::mpsc::Sender<WorkerCmd>,
    event_log: Arc<Mutex<Vec<String>>>,
    _worker: kithara_platform::JoinHandle<()>,
    cache: CachedState,
}

#[wasm_bindgen]
impl WasmPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        info!("WasmPlayer: initialising worker session channel");

        // 1. Initialise the session channel BEFORE spawning the Worker.
        wasm_support::ensure_main_session();
        wasm_support::init_worker_session();

        // 2. Create command channel.
        let (cmd_tx, cmd_rx) = kithara_platform::sync::mpsc::channel();
        let event_log = Arc::new(Mutex::new(Vec::new()));

        // 3. Spawn the engine Worker.
        let log = Arc::clone(&event_log);
        let worker = kithara_platform::spawn(move || {
            crate::worker_entry::worker_main(cmd_rx, log);
        });

        info!("WasmPlayer: engine worker spawned");

        Self {
            cmd_tx,
            event_log,
            _worker: worker,
            cache: CachedState::new(),
        }
    }

    // -- Commands forwarded to Worker ------------------------------------

    pub async fn select_track(&self, url: String) -> Result<(), JsValue> {
        clog!("[PLAYER] select_track: sending to Worker url={url}");

        let (reply_tx, reply_rx) = kithara_platform::tokio::sync::oneshot::channel();
        self.cmd_tx
            .send_sync(WorkerCmd::SelectTrack { url, reply_tx })
            .map_err(|_| js_error("worker channel closed"))?;

        let result = reply_rx
            .await
            .map_err(|_| js_error("worker reply dropped"))?;

        result.map_err(|e| js_error(format!("select_track failed: {e}")))?;

        clog!("[PLAYER] select_track: complete");
        Ok(())
    }

    pub fn play(&self) {
        let _ = self.cmd_tx.send_sync(WorkerCmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.cmd_tx.send_sync(WorkerCmd::Pause);
    }

    pub fn stop(&self) {
        let _ = self.cmd_tx.send_sync(WorkerCmd::Stop);
    }

    pub fn seek(&self, position_ms: f64) -> Result<(), JsValue> {
        self.cmd_tx
            .send_sync(WorkerCmd::Seek(position_ms))
            .map_err(|_| js_error("worker channel closed"))
    }

    pub fn set_volume(&self, volume: f32) {
        self.cache.volume.set(volume);
        let _ = self.cmd_tx.send_sync(WorkerCmd::SetVolume(volume));
    }

    pub fn set_crossfade_seconds(&self, seconds: f32) {
        self.cache.crossfade_secs.set(seconds);
        let _ = self.cmd_tx.send_sync(WorkerCmd::SetCrossfade(seconds));
    }

    pub fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        {
            let mut gains = self.cache.eq_gains.borrow_mut();
            if let Some(slot) = gains.get_mut(band as usize) {
                *slot = gain_db;
            }
        }
        self.cmd_tx
            .send_sync(WorkerCmd::SetEqGain { band, gain_db })
            .map_err(|_| js_error("worker channel closed"))
    }

    pub fn reset_eq(&self) -> Result<(), JsValue> {
        {
            let mut gains = self.cache.eq_gains.borrow_mut();
            for g in gains.iter_mut() {
                *g = 0.0;
            }
        }
        self.cmd_tx
            .send_sync(WorkerCmd::ResetEq)
            .map_err(|_| js_error("worker channel closed"))
    }

    pub fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        self.cache.ducking.set(mode);
        self.cmd_tx
            .send_sync(WorkerCmd::SetDucking(mode))
            .map_err(|_| js_error("worker channel closed"))
    }

    // -- Status reads from shared atomics (no Worker round-trip) ----------

    pub fn get_position_ms(&self) -> f64 {
        wasm_support::bridge_position_secs() * 1000.0
    }

    pub fn get_duration_ms(&self) -> f64 {
        wasm_support::bridge_duration_secs() * 1000.0
    }

    pub fn is_playing(&self) -> bool {
        wasm_support::bridge_is_playing()
    }

    pub fn process_count(&self) -> f64 {
        wasm_support::bridge_process_count() as f64
    }

    // -- Cached getters (no Worker round-trip) ----------------------------

    pub fn get_volume(&self) -> f32 {
        self.cache.volume.get()
    }

    pub fn get_crossfade_seconds(&self) -> f32 {
        self.cache.crossfade_secs.get()
    }

    pub fn eq_band_count(&self) -> u32 {
        self.cache.eq_bands
    }

    pub fn eq_gain(&self, band: u32) -> f32 {
        let gains = self.cache.eq_gains.borrow();
        gains.get(band as usize).copied().unwrap_or(0.0)
    }

    pub fn get_session_ducking(&self) -> u32 {
        self.cache.ducking.get()
    }

    // -- Tick (main thread graph update) ----------------------------------

    /// Poll session commands from Workers and update the audio graph.
    ///
    /// Must be called unconditionally — even before a track is selected —
    /// because `select_track` sends session-engine commands from the Worker
    /// that the main thread must process for the call to complete.
    pub fn tick(&self) {
        wasm_support::tick_and_poll();
    }

    // -- Events -----------------------------------------------------------

    pub fn take_events(&self) -> String {
        let mut events = self.event_log.lock_sync();
        if events.is_empty() {
            return String::new();
        }
        let out = events.join("\n");
        events.clear();
        out
    }
}
