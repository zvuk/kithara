//! Worker-based WASM player.
//!
//! `Player` is a normal Rust struct with methods. The `#[wasm_export]` proc
//! macro on the impl block generates `#[wasm_bindgen]` free functions
//! (`player_play`, `player_seek`, etc.) for every method marked `#[export]`.

use std::sync::{
    LazyLock,
    atomic::{AtomicU32, Ordering},
};

use js_sys::Promise;
use kithara_platform::sync::{Mutex, MutexGuard, mpsc};
use kithara_play::wasm_support;
use kithara_wasm_macros::wasm_export;
use tracing::info;
use wasm_bindgen::prelude::*;

use crate::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

struct Player {
    volume: AtomicU32,
    crossfade_secs: AtomicU32,
    eq_gains: [AtomicU32; Player::EQ_BANDS],
    ducking: AtomicU32,
    cmd_tx: LazyLock<Mutex<Option<mpsc::Sender<WorkerCmd>>>>,
    start_lock: LazyLock<Mutex<()>>,
    start_count: AtomicU32,
}

static PLAYER: Player = Player {
    volume: AtomicU32::new(0),
    crossfade_secs: AtomicU32::new(0),
    eq_gains: [const { AtomicU32::new(0) }; Player::EQ_BANDS],
    ducking: AtomicU32::new(0),
    cmd_tx: LazyLock::new(|| Mutex::new(None)),
    start_lock: LazyLock::new(|| Mutex::new(())),
    start_count: AtomicU32::new(0),
};

fn player() -> &'static Player {
    &PLAYER
}

impl Player {
    const CROSSFADE_SECONDS: f32 = 5.0;
    const EQ_BANDS: usize = 10;

    /// Default initial volume.
    const DEFAULT_VOLUME: f32 = 0.5;

    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    fn load_f32(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }

    fn store_f32(a: &AtomicU32, val: f32) {
        a.store(val.to_bits(), Ordering::Relaxed);
    }

    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock_sync()
    }

    fn find_player_idx(&self) -> Option<usize> {
        None
    }

    fn clear_cmd_tx(&self) {
        *self.lock_cmd_tx() = None;
    }

    fn ensure_worker_started(&self) -> Result<(), JsValue> {
        if self.lock_cmd_tx().is_some() {
            return Ok(());
        }

        let _start_guard = self.start_lock.lock_sync();

        if self.lock_cmd_tx().is_some() {
            return Ok(());
        }

        let start_no = self.start_count.fetch_add(1, Ordering::Relaxed) + 1;
        info!("player_new: initialising worker start={start_no}");

        wasm_support::ensure_main_session();
        wasm_support::init_worker_session();
        crate::js_channel::init_event_reader();
        self.reset_cached_state();

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = kithara_platform::spawn(move || {
            crate::worker_entry::worker_main(cmd_rx);
        });
        std::mem::forget(worker);

        Ok(())
    }

    fn reset_cached_state(&self) {
        Self::store_f32(&self.volume, Self::DEFAULT_VOLUME);
        Self::store_f32(&self.crossfade_secs, Self::CROSSFADE_SECONDS);
        for gain in &self.eq_gains {
            Self::store_f32(gain, 0.0);
        }
        self.ducking.store(0, Ordering::Relaxed);
    }
}

#[wasm_export]
impl Player {
    fn send_cmd(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        self.ensure_worker_started()?;

        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;

        if tx.send_sync(cmd.clone()).is_ok() {
            return Ok(());
        }

        // If worker channel closed, clear sender, restart worker, and retry once.
        self.clear_cmd_tx();
        self.ensure_worker_started()?;
        let tx = self
            .lock_cmd_tx()
            .as_ref()
            .cloned()
            .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
        tx.send_sync(cmd)
            .map_err(|_| JsValue::from_str("worker channel closed"))
    }

    #[export]
    fn select_track(&self, url: String) -> Result<js_sys::Promise, JsValue> {
        clog!("[PLAYER] select_track: sending to Worker url={url}");
        let request_id = crate::js_channel::next_request_id();
        let cmd = WorkerCmd::SelectTrack { url, request_id };
        self.send_cmd(cmd)?;
        crate::js_channel::reply_promise(request_id)
    }

    #[export]
    fn warm_up_audio(&self) {
        wasm_support::warm_up_audio();
    }

    #[export]
    fn play(&self) {
        let _ = self.send_cmd(WorkerCmd::Play);
    }

    #[export]
    fn pause(&self) {
        let _ = self.send_cmd(WorkerCmd::Pause);
    }

    #[export]
    fn stop(&self) {
        let _ = self.send_cmd(WorkerCmd::Stop);
    }

    #[export]
    fn seek(&self, position_ms: f64) -> Result<(), JsValue> {
        self.send_cmd(WorkerCmd::Seek(position_ms))
    }

    #[export]
    fn set_volume(&self, volume: f32) {
        Self::store_f32(&self.volume, volume);
        let _ = self.send_cmd(WorkerCmd::SetVolume(volume));
    }

    #[export]
    fn set_crossfade_seconds(&self, seconds: f32) {
        Self::store_f32(&self.crossfade_secs, seconds);
        let _ = self.send_cmd(WorkerCmd::SetCrossfade(seconds));
    }

    #[export]
    fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            Self::store_f32(slot, gain_db);
        }
        self.send_cmd(WorkerCmd::SetEqGain { band, gain_db })
    }

    #[export]
    fn reset_eq(&self) -> Result<(), JsValue> {
        for g in &self.eq_gains {
            Self::store_f32(g, 0.0);
        }
        self.send_cmd(WorkerCmd::ResetEq)
    }

    #[export]
    fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        self.ducking.store(mode, Ordering::Relaxed);
        self.send_cmd(WorkerCmd::SetDucking(mode))
    }

    #[export]
    fn get_position_ms(&self) -> f64 {
        wasm_support::bridge_position_secs() * Self::MS_PER_SECOND
    }

    #[export]
    fn get_duration_ms(&self) -> f64 {
        wasm_support::bridge_duration_secs() * Self::MS_PER_SECOND
    }

    #[export]
    fn is_playing(&self) -> bool {
        wasm_support::bridge_is_playing()
    }

    #[export]
    fn process_count(&self) -> f64 {
        wasm_support::bridge_process_count() as f64
    }

    #[export]
    fn get_volume(&self) -> f32 {
        Self::load_f32(&self.volume)
    }

    #[export]
    fn get_crossfade_seconds(&self) -> f32 {
        Self::load_f32(&self.crossfade_secs)
    }

    #[export]
    fn eq_band_count(&self) -> u32 {
        Self::EQ_BANDS as u32
    }

    #[export]
    fn eq_gain(&self, band: u32) -> f32 {
        self.eq_gains
            .get(band as usize)
            .map(Self::load_f32)
            .unwrap_or(0.0)
    }

    #[export]
    fn get_session_ducking(&self) -> u32 {
        self.ducking.load(Ordering::Relaxed)
    }

    #[export]
    fn tick(&self) {
        wasm_support::tick_and_poll();
    }

    #[export]
    fn take_events(&self) -> String {
        crate::js_channel::take_events()
    }
}

/// Returns a `Promise` that resolves after worker spawn.
#[allow(unreachable_pub)]
#[wasm_bindgen]
pub fn player_new() -> Promise {
    if let Err(err) = player().ensure_worker_started() {
        return Promise::reject(&err);
    }
    Promise::resolve(&JsValue::UNDEFINED)
}
