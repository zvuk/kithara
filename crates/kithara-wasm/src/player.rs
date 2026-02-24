//! WASM player powered by `kithara-play`.

use std::sync::Arc;

use kithara_platform::Mutex;
use kithara_play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig, SessionDuckingMode};
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{info, warn};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// Direct console.log that bypasses tracing infrastructure.
macro_rules! clog {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into());
    };
}

/// Diagnostic: return current WASM linear memory size in bytes.
#[wasm_bindgen]
pub fn wasm_memory_bytes() -> f64 {
    let mem: js_sys::WebAssembly::Memory = wasm_bindgen::memory().unchecked_into();
    let buf: js_sys::ArrayBuffer = mem.buffer().unchecked_into();
    buf.byte_length() as f64
}

const CROSSFADE_SECONDS: f32 = 5.0;
const FILE_URL_DEFAULT: &str = "https://stream.silvercomet.top/track.mp3";
const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";

fn js_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

#[wasm_bindgen]
pub struct WasmPlayer {
    current_index: Mutex<Option<usize>>,
    event_log: Arc<Mutex<Vec<String>>>,
    player: PlayerImpl,
    playlist: Vec<String>,
}

#[wasm_bindgen]
impl WasmPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        info!("WasmPlayer created");
        let player =
            PlayerImpl::new(PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS));
        let event_log = Arc::new(Mutex::new(Vec::new()));
        Self {
            current_index: Mutex::new(None),
            event_log,
            player,
            playlist: vec![FILE_URL_DEFAULT.to_string(), HLS_URL_DEFAULT.to_string()],
        }
    }

    pub fn default_file_url() -> String {
        FILE_URL_DEFAULT.to_string()
    }

    pub fn default_hls_url() -> String {
        HLS_URL_DEFAULT.to_string()
    }

    pub fn playlist_len(&self) -> u32 {
        self.playlist.len() as u32
    }

    pub fn playlist_item(&self, index: u32) -> Result<String, JsValue> {
        let idx = usize::try_from(index)
            .map_err(|_| js_error(format!("playlist index is too large: {index}")))?;
        self.playlist
            .get(idx)
            .cloned()
            .ok_or_else(|| js_error(format!("playlist index out of range: {idx}")))
    }

    pub fn current_index(&self) -> i32 {
        self.current_index.lock().map_or(-1, |idx| idx as i32)
    }

    pub fn add_track(&mut self, url: String) -> Result<u32, JsValue> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(js_error("track URL is empty"));
        }

        ResourceConfig::new(trimmed)
            .map_err(|err| js_error(format!("invalid track URL: {err}")))?;

        self.playlist.push(trimmed.to_string());
        Ok((self.playlist.len() - 1) as u32)
    }

    pub async fn select_track(&self, index: u32) -> Result<(), JsValue> {
        let idx = usize::try_from(index)
            .map_err(|_| js_error(format!("playlist index is too large: {index}")))?;
        let Some(url) = self.playlist.get(idx).cloned() else {
            return Err(js_error(format!("playlist index out of range: {idx}")));
        };

        clog!("[TRACE] select_track: loading resource url={url}");
        let resource = load_resource(&url).await?;
        clog!("[TRACE] select_track: resource loaded, subscribing events");
        log_resource_events(
            resource.subscribe(),
            url.clone(),
            Arc::clone(&self.event_log),
        );
        clog!("[TRACE] select_track: calling play_resource");
        self.player
            .play_resource(resource)
            .map_err(|err| js_error(format!("failed to select track: {err}")))?;
        clog!("[TRACE] select_track: play_resource done, updating index");
        *self.current_index.lock() = Some(idx);
        clog!("[TRACE] select_track: complete");

        Ok(())
    }

    pub fn play(&mut self) {
        self.player.play();
    }

    pub fn pause(&mut self) {
        self.player.pause();
    }

    pub fn stop(&mut self) {
        self.player.pause();
        let _ = self.player.seek_seconds(0.0);
    }

    pub fn seek(&mut self, position_ms: f64) -> Result<(), JsValue> {
        self.player
            .seek_seconds(position_ms.max(0.0) / 1000.0)
            .map_err(|err| js_error(format!("seek failed: {err}")))
    }

    pub fn get_position_ms(&self) -> f64 {
        self.player.position_seconds().map_or(0.0, |s| s * 1000.0)
    }

    pub fn tick(&self) -> Result<(), JsValue> {
        if self.current_index() < 0 {
            return Ok(());
        }

        self.player
            .tick()
            .map_err(|err| js_error(format!("tick failed: {err}")))?;

        for notification in self.player.drain_notifications() {
            let line = format!("player notification={notification}");
            clog!("[TRACE] tick: {line}");
            push_event(&self.event_log, line);
        }

        Ok(())
    }

    pub fn get_duration_ms(&self) -> f64 {
        self.player.duration_seconds().map_or(0.0, |s| s * 1000.0)
    }

    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    /// Diagnostic: how many times the audio-thread process() callback has run.
    pub fn process_count(&self) -> f64 {
        self.player.process_count() as f64
    }

    pub fn get_volume(&self) -> f32 {
        self.player.volume()
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }

    pub fn get_crossfade_seconds(&self) -> f32 {
        self.player.crossfade_duration()
    }

    pub fn set_crossfade_seconds(&self, seconds: f32) {
        self.player.set_crossfade_duration(seconds);
    }

    pub fn eq_band_count(&self) -> u32 {
        self.player.eq_band_count() as u32
    }

    pub fn eq_gain(&self, band: u32) -> f32 {
        self.player.eq_gain(band as usize).unwrap_or(0.0)
    }

    pub fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        self.player
            .set_eq_gain(band as usize, gain_db)
            .map_err(|err| js_error(format!("set_eq_gain failed: {err}")))
    }

    pub fn reset_eq(&self) -> Result<(), JsValue> {
        self.player
            .reset_eq()
            .map_err(|err| js_error(format!("reset_eq failed: {err}")))
    }

    pub fn get_session_ducking(&self) -> u32 {
        match self.player.session_ducking() {
            SessionDuckingMode::Off => 0,
            SessionDuckingMode::Soft => 1,
            SessionDuckingMode::Hard => 2,
            _ => 0,
        }
    }

    pub fn set_session_ducking(&self, mode: u32) -> Result<(), JsValue> {
        let mode = match mode {
            0 => SessionDuckingMode::Off,
            1 => SessionDuckingMode::Soft,
            2 => SessionDuckingMode::Hard,
            _ => return Err(js_error(format!("invalid ducking mode: {mode}"))),
        };
        self.player
            .set_session_ducking(mode)
            .map_err(|err| js_error(format!("set_session_ducking failed: {err}")))
    }

    pub fn take_events(&self) -> String {
        let mut events = self.event_log.lock();
        if events.is_empty() {
            return String::new();
        }
        let out = events.join("\n");
        events.clear();
        out
    }
}

async fn load_resource(url: &str) -> Result<Resource, JsValue> {
    let config = ResourceConfig::new(url).map_err(|err| js_error(format!("invalid URL: {err}")))?;
    Resource::new(config)
        .await
        .map_err(|err| js_error(format!("failed to load resource: {err}")))
}

fn log_resource_events<T>(
    mut events_rx: broadcast::Receiver<T>,
    url: String,
    event_log: Arc<Mutex<Vec<String>>>,
) where
    T: core::fmt::Debug + Clone + Send + 'static,
{
    spawn_local(async move {
        loop {
            match events_rx.recv().await {
                Ok(ev) => {
                    let event_dbg = format!("{ev:?}");
                    let line = format!("resource src={url} event={event_dbg}");
                    info!("{line}");
                    push_event(&event_log, line);
                }
                Err(RecvError::Lagged(n)) => {
                    let line = format!("resource src={url} events lagged n={n}");
                    warn!("{line}");
                    push_event(&event_log, line);
                }
                Err(RecvError::Closed) => {
                    let line = format!("resource src={url} event stream closed");
                    warn!("{line}");
                    push_event(&event_log, line);
                    break;
                }
            }
        }
    });
}

fn push_event(event_log: &Arc<Mutex<Vec<String>>>, line: String) {
    let mut events = event_log.lock();
    events.push(line);
    const MAX_EVENTS: usize = 1024;
    if events.len() > MAX_EVENTS {
        let keep_from = events.len() - MAX_EVENTS;
        events.drain(0..keep_from);
    }
}
