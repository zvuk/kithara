//! Worker-based WASM player.
//!
//! `Player` is a normal Rust struct with methods. The `#[wasm_export]` proc
//! macro on the impl block generates `#[wasm_bindgen]` free functions
//! (`player_play`, `player_seek`, etc.) for every method marked `#[export]`.

use std::sync::{
    LazyLock,
    atomic::{AtomicU32, Ordering},
};

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

const CROSSFADE_SECONDS: f32 = 5.0;
const EQ_BANDS: usize = 10;

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, val: f32) {
    a.store(val.to_bits(), Ordering::Relaxed);
}

struct Player;

static PLAYER: Player = Player;

// Cached settings used by getters on the JS side.
static VOLUME: AtomicU32 = AtomicU32::new(0);
static CROSSFADE_SECS: AtomicU32 = AtomicU32::new(0);
static EQ_GAINS: [AtomicU32; EQ_BANDS] = [const { AtomicU32::new(0) }; EQ_BANDS];
static DUCKING: AtomicU32 = AtomicU32::new(0);

static CMD_TX: LazyLock<Mutex<Option<mpsc::Sender<WorkerCmd>>>> =
    LazyLock::new(|| Mutex::new(None));
static START_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static START_COUNT: AtomicU32 = AtomicU32::new(0);

fn player() -> &'static Player {
    &PLAYER
}

fn lock_cmd_tx() -> MutexGuard<'static, Option<mpsc::Sender<WorkerCmd>>> {
    CMD_TX.lock_sync()
}

fn ensure_worker_started() -> Result<(), JsValue> {
    if lock_cmd_tx().is_some() {
        return Ok(());
    }

    let _start_guard = START_LOCK.lock_sync();

    if lock_cmd_tx().is_some() {
        return Ok(());
    }

    let start_no = START_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    info!("player_new: initialising worker start={start_no}");

    wasm_support::ensure_main_session();
    wasm_support::init_worker_session();
    crate::js_channel::init_event_reader();
    reset_cached_state();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    *lock_cmd_tx() = Some(cmd_tx);

    let worker = kithara_platform::spawn(move || {
        crate::worker_entry::worker_main(cmd_rx);
    });
    std::mem::forget(worker);

    Ok(())
}

fn clear_cmd_tx() {
    *lock_cmd_tx() = None;
}

fn send_cmd(cmd: WorkerCmd) -> Result<(), JsValue> {
    ensure_worker_started()?;

    let tx = lock_cmd_tx()
        .as_ref()
        .cloned()
        .ok_or_else(|| JsValue::from_str("command channel not ready"))?;

    if tx.send_sync(cmd.clone()).is_ok() {
        return Ok(());
    }

    // If worker channel closed, clear sender, restart worker, and retry once.
    clear_cmd_tx();
    ensure_worker_started()?;
    let tx = lock_cmd_tx()
        .as_ref()
        .cloned()
        .ok_or_else(|| JsValue::from_str("command channel not ready"))?;
    tx.send_sync(cmd)
        .map_err(|_| JsValue::from_str("worker channel closed"))
}

fn reset_cached_state() {
    store_f32(&VOLUME, 0.5);
    store_f32(&CROSSFADE_SECS, CROSSFADE_SECONDS);
    for gain in &EQ_GAINS {
        store_f32(gain, 0.0);
    }
    DUCKING.store(0, Ordering::Relaxed);
}

#[wasm_export]
impl Player {
    fn send_cmd(&self, cmd: WorkerCmd) -> Result<(), JsValue> {
        send_cmd(cmd)
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
        store_f32(&VOLUME, volume);
        let _ = self.send_cmd(WorkerCmd::SetVolume(volume));
    }

    #[export]
    fn set_crossfade_seconds(&self, seconds: f32) {
        store_f32(&CROSSFADE_SECS, seconds);
        let _ = self.send_cmd(WorkerCmd::SetCrossfade(seconds));
    }

    #[export]
    fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        if let Some(slot) = EQ_GAINS.get(band as usize) {
            store_f32(slot, gain_db);
        }
        self.send_cmd(WorkerCmd::SetEqGain { band, gain_db })
    }

    #[export]
    fn reset_eq(&self) -> Result<(), JsValue> {
        for g in &EQ_GAINS {
            store_f32(g, 0.0);
        }
        self.send_cmd(WorkerCmd::ResetEq)
    }

    #[export]
    fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        DUCKING.store(mode, Ordering::Relaxed);
        self.send_cmd(WorkerCmd::SetDucking(mode))
    }

    #[export]
    fn get_position_ms(&self) -> f64 {
        wasm_support::bridge_position_secs() * 1000.0
    }

    #[export]
    fn get_duration_ms(&self) -> f64 {
        wasm_support::bridge_duration_secs() * 1000.0
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
        load_f32(&VOLUME)
    }

    #[export]
    fn get_crossfade_seconds(&self) -> f32 {
        load_f32(&CROSSFADE_SECS)
    }

    #[export]
    fn eq_band_count(&self) -> u32 {
        EQ_BANDS as u32
    }

    #[export]
    fn eq_gain(&self, band: u32) -> f32 {
        EQ_GAINS.get(band as usize).map(load_f32).unwrap_or(0.0)
    }

    #[export]
    fn get_session_ducking(&self) -> u32 {
        DUCKING.load(Ordering::Relaxed)
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
pub fn player_new() -> js_sys::Promise {
    if let Err(err) = ensure_worker_started() {
        return js_sys::Promise::reject(&err);
    }
    js_sys::Promise::resolve(&JsValue::UNDEFINED)
}
