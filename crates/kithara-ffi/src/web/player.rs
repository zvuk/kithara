use std::sync::{
    LazyLock,
    atomic::{AtomicU32, Ordering},
};

use js_sys::Promise;
use kithara_platform::sync::{Mutex, MutexGuard, mpsc};
use kithara_play::wasm_support;
use tracing::info;
use wasm_bindgen::prelude::*;

use crate::web::commands::WorkerCmd;

macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

struct Player {
    crossfade_secs: AtomicU32,
    ducking: AtomicU32,
    start_count: AtomicU32,
    volume: AtomicU32,
    cmd_tx: LazyLock<Mutex<Option<mpsc::Sender<WorkerCmd>>>>,
    start_lock: LazyLock<Mutex<()>>,
    eq_gains: [AtomicU32; Player::EQ_BANDS],
}

static PLAYER: Player = Player {
    crossfade_secs: AtomicU32::new(0),
    ducking: AtomicU32::new(0),
    start_count: AtomicU32::new(0),
    volume: AtomicU32::new(0),
    cmd_tx: LazyLock::new(|| Mutex::new(None)),
    start_lock: LazyLock::new(|| Mutex::new(())),
    eq_gains: [const { AtomicU32::new(0) }; Player::EQ_BANDS],
};

fn player() -> &'static Player {
    &PLAYER
}

impl Player {
    const CROSSFADE_SECONDS: f32 = 5.0;
    /// Default initial volume.
    const DEFAULT_VOLUME: f32 = 0.5;
    const EQ_BANDS: usize = 10;
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

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
        crate::web::js::init_event_reader();
        self.reset_cached_state();

        let (cmd_tx, cmd_rx) = mpsc::channel();
        *self.lock_cmd_tx() = Some(cmd_tx);

        let worker = kithara_platform::spawn(move || {
            crate::web::worker::worker_main(cmd_rx);
        });
        std::mem::forget(worker);

        Ok(())
    }

    fn eq_band_count(&self) -> u32 {
        Self::EQ_BANDS as u32
    }

    fn eq_gain(&self, band: u32) -> f32 {
        self.eq_gains
            .get(band as usize)
            .map(Self::load_f32)
            .unwrap_or(0.0)
    }

    fn get_crossfade_seconds(&self) -> f32 {
        Self::load_f32(&self.crossfade_secs)
    }

    fn get_duration_ms(&self) -> f64 {
        wasm_support::bridge_duration_secs() * Self::MS_PER_SECOND
    }

    fn get_position_ms(&self) -> f64 {
        wasm_support::bridge_position_secs() * Self::MS_PER_SECOND
    }

    fn get_session_ducking(&self) -> u32 {
        self.ducking.load(Ordering::Relaxed)
    }

    fn get_volume(&self) -> f32 {
        Self::load_f32(&self.volume)
    }

    fn is_playing(&self) -> bool {
        wasm_support::bridge_is_playing()
    }

    fn load_f32(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }

    fn lock_cmd_tx(&self) -> MutexGuard<'_, Option<mpsc::Sender<WorkerCmd>>> {
        self.cmd_tx.lock_sync()
    }

    fn pause(&self) {
        let _ = self.send_cmd(WorkerCmd::Pause);
    }

    fn play(&self) {
        let _ = self.send_cmd(WorkerCmd::Play);
    }

    fn process_count(&self) -> f64 {
        wasm_support::bridge_process_count() as f64
    }

    fn reset_cached_state(&self) {
        Self::store_f32(&self.volume, Self::DEFAULT_VOLUME);
        Self::store_f32(&self.crossfade_secs, Self::CROSSFADE_SECONDS);
        for gain in &self.eq_gains {
            Self::store_f32(gain, 0.0);
        }
        self.ducking.store(0, Ordering::Relaxed);
    }

    fn reset_eq(&self) -> Result<(), JsValue> {
        for g in &self.eq_gains {
            Self::store_f32(g, 0.0);
        }
        self.send_cmd(WorkerCmd::ResetEq)
    }

    fn seek(&self, position_ms: f64) -> Result<(), JsValue> {
        self.send_cmd(WorkerCmd::Seek(position_ms))
    }

    fn select_track(&self, url: String) -> Result<Promise, JsValue> {
        clog!("[PLAYER] select_track: sending to Worker url={url}");
        let request_id = crate::web::js::next_request_id();
        let cmd = WorkerCmd::SelectTrack { url, request_id };
        self.send_cmd(cmd)?;
        crate::web::js::reply_promise(request_id)
    }

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

    fn set_crossfade_seconds(&self, seconds: f32) {
        Self::store_f32(&self.crossfade_secs, seconds);
        let _ = self.send_cmd(WorkerCmd::SetCrossfade(seconds));
    }

    fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            Self::store_f32(slot, gain_db);
        }
        self.send_cmd(WorkerCmd::SetEqGain { band, gain_db })
    }

    fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        self.ducking.store(mode, Ordering::Relaxed);
        self.send_cmd(WorkerCmd::SetDucking(mode))
    }

    fn set_volume(&self, volume: f32) {
        Self::store_f32(&self.volume, volume);
        let _ = self.send_cmd(WorkerCmd::SetVolume(volume));
    }

    fn stop(&self) {
        let _ = self.send_cmd(WorkerCmd::Stop);
    }

    fn store_f32(a: &AtomicU32, val: f32) {
        a.store(val.to_bits(), Ordering::Relaxed);
    }

    fn take_events(&self) -> String {
        crate::web::js::take_events()
    }

    fn tick(&self) {
        wasm_support::tick_and_poll();
    }

    fn warm_up_audio(&self) {
        wasm_support::warm_up_audio();
    }
}

#[wasm_bindgen]
pub fn player_eq_band_count() -> u32 {
    player().eq_band_count()
}

#[wasm_bindgen]
pub fn player_eq_gain(band: u32) -> f32 {
    player().eq_gain(band)
}

#[wasm_bindgen]
pub fn player_get_crossfade_seconds() -> f32 {
    player().get_crossfade_seconds()
}

#[wasm_bindgen]
pub fn player_get_duration_ms() -> f64 {
    player().get_duration_ms()
}

#[wasm_bindgen]
pub fn player_get_position_ms() -> f64 {
    player().get_position_ms()
}

#[wasm_bindgen]
pub fn player_get_session_ducking() -> u32 {
    player().get_session_ducking()
}

#[wasm_bindgen]
pub fn player_get_volume() -> f32 {
    player().get_volume()
}

#[wasm_bindgen]
pub fn player_is_playing() -> bool {
    player().is_playing()
}

#[wasm_bindgen]
pub fn player_pause() {
    player().pause()
}

#[wasm_bindgen]
pub fn player_play() {
    player().play()
}

#[wasm_bindgen]
pub fn player_process_count() -> f64 {
    player().process_count()
}

#[wasm_bindgen]
pub fn player_reset_eq() -> Result<(), JsValue> {
    player().reset_eq()
}

#[wasm_bindgen]
pub fn player_seek(position_ms: f64) -> Result<(), JsValue> {
    player().seek(position_ms)
}

#[wasm_bindgen]
pub fn player_select_track(url: String) -> Result<Promise, JsValue> {
    player().select_track(url)
}

#[wasm_bindgen]
pub fn player_set_crossfade_seconds(seconds: f32) {
    player().set_crossfade_seconds(seconds)
}

#[wasm_bindgen]
pub fn player_set_eq_gain(band: u32, gain_db: f32) -> Result<(), JsValue> {
    player().set_eq_gain(band, gain_db)
}

#[wasm_bindgen]
pub fn player_set_session_ducking(mode: u32) -> Result<(), JsValue> {
    player().set_session_ducking(mode)
}

#[wasm_bindgen]
pub fn player_set_volume(volume: f32) {
    player().set_volume(volume)
}

#[wasm_bindgen]
pub fn player_stop() {
    player().stop()
}

#[wasm_bindgen]
pub fn player_take_events() -> String {
    player().take_events()
}

#[wasm_bindgen]
pub fn player_tick() {
    player().tick()
}

#[wasm_bindgen]
pub fn player_warm_up_audio() {
    player().warm_up_audio()
}

/// Returns a `Promise` that resolves after worker spawn.
#[cfg_attr(target_family = "wasm", allow(unreachable_pub))]
#[wasm_bindgen]
pub fn player_new() -> Promise {
    if let Err(err) = player().ensure_worker_started() {
        return Promise::reject(&err);
    }
    Promise::resolve(&JsValue::UNDEFINED)
}
