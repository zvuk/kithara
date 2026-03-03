//! Worker-based WASM player — struct API.
//!
//! Exported as `#[wasm_bindgen]` struct [`Player`]. The `Rc<WasmRefCell<…>>`
//! wrapper that wasm-bindgen creates is safe because `firewheel-web-audio`
//! passes `thread_stack_size` to `initSync`, preventing Worker TLS init from
//! corrupting the WASM heap.
//!
//! Getter values (volume, crossfade, EQ, ducking) are cached inside the struct
//! so that reads never require a Worker round-trip.

use std::{cell::Cell, sync::Arc};

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

// ── Exported player struct ──────────────────────────────────────────

#[wasm_bindgen]
pub struct Player {
    cmd_tx: kithara_platform::sync::mpsc::Sender<WorkerCmd>,
    event_log: Arc<Mutex<Vec<String>>>,
    // Cached settings (main-thread mirror, no Worker round-trip)
    volume: Cell<f32>,
    crossfade_secs: Cell<f32>,
    eq_gains: [Cell<f32>; EQ_BANDS],
    ducking: Cell<u32>,
}

#[wasm_bindgen]
impl Player {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Player {
        info!("Player::new: initialising worker session channel");

        wasm_support::ensure_main_session();
        wasm_support::init_worker_session();

        let (cmd_tx, cmd_rx) = kithara_platform::sync::mpsc::channel();
        let event_log = Arc::new(Mutex::new(Vec::new()));
        let worker_log = Arc::clone(&event_log);

        let _worker = kithara_platform::spawn(move || {
            crate::worker_entry::worker_main(cmd_rx, worker_log);
        });

        info!("Player::new: engine worker spawned");

        Player {
            cmd_tx,
            event_log,
            volume: Cell::new(0.5),
            crossfade_secs: Cell::new(CROSSFADE_SECONDS),
            eq_gains: [const { Cell::new(0.0) }; EQ_BANDS],
            ducking: Cell::new(0),
        }
    }

    // -- Track loading ────────────────────────────────────────────

    pub async fn select_track(&self, url: String) -> Result<(), JsValue> {
        clog!("[PLAYER] select_track: sending to Worker url={url}");

        let (reply_tx, reply_rx) = kithara_platform::tokio::sync::oneshot::channel();
        self.send_cmd(WorkerCmd::SelectTrack { url, reply_tx })?;

        let result = reply_rx
            .await
            .map_err(|_| js_error("worker reply dropped"))?;

        result.map_err(|e| js_error(format!("select_track failed: {e}")))?;

        clog!("[PLAYER] select_track: complete");
        Ok(())
    }

    // -- Audio context ────────────────────────────────────────────

    pub fn warm_up_audio(&self) {
        wasm_support::warm_up_audio();
    }

    // -- Transport ────────────────────────────────────────────────

    pub fn play(&self) {
        let _ = self.send_cmd(WorkerCmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.send_cmd(WorkerCmd::Pause);
    }

    pub fn stop(&self) {
        let _ = self.send_cmd(WorkerCmd::Stop);
    }

    pub fn seek(&self, position_ms: f64) -> Result<(), JsValue> {
        self.send_cmd(WorkerCmd::Seek(position_ms))
    }

    // -- Settings ─────────────────────────────────────────────────

    pub fn set_volume(&self, volume: f32) {
        self.volume.set(volume);
        let _ = self.send_cmd(WorkerCmd::SetVolume(volume));
    }

    pub fn set_crossfade_seconds(&self, seconds: f32) {
        self.crossfade_secs.set(seconds);
        let _ = self.send_cmd(WorkerCmd::SetCrossfade(seconds));
    }

    pub fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            slot.set(gain_db);
        }
        self.send_cmd(WorkerCmd::SetEqGain { band, gain_db })
    }

    pub fn reset_eq(&self) -> Result<(), JsValue> {
        for g in &self.eq_gains {
            g.set(0.0);
        }
        self.send_cmd(WorkerCmd::ResetEq)
    }

    pub fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        self.ducking.set(mode);
        self.send_cmd(WorkerCmd::SetDucking(mode))
    }

    // -- Status reads from shared atomics (no Worker round-trip) ──

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

    // -- Cached getters (no Worker round-trip) ────────────────────

    pub fn get_volume(&self) -> f32 {
        self.volume.get()
    }

    pub fn get_crossfade_seconds(&self) -> f32 {
        self.crossfade_secs.get()
    }

    pub fn eq_band_count(&self) -> u32 {
        EQ_BANDS as u32
    }

    pub fn eq_gain(&self, band: u32) -> f32 {
        self.eq_gains
            .get(band as usize)
            .map(Cell::get)
            .unwrap_or(0.0)
    }

    pub fn get_session_ducking(&self) -> u32 {
        self.ducking.get()
    }

    // -- Tick (main thread graph update) ──────────────────────────

    pub fn tick(&self) {
        wasm_support::tick_and_poll();
    }

    // -- Events ──────────────────────────────────────────────────

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

// ── Private helpers ─────────────────────────────────────────────────

impl Player {
    fn send_cmd(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.cmd_tx
            .send_sync(cmd)
            .map_err(|_| js_error("worker channel closed"))
    }
}
