//! WASM player powered by `kithara-play`.

use kithara_play::{PlayerConfig, PlayerImpl, Resource, ResourceConfig};
use tracing::info;
use wasm_bindgen::prelude::*;

const CROSSFADE_SECONDS: f32 = 5.0;
const FILE_URL_DEFAULT: &str = "http://www.hyperion-records.co.uk/audiotest/14 Clementi Piano Sonata in D major, Op 25 No \
     6 - Movement 2 Un poco andante.MP3";
const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";

fn js_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

#[wasm_bindgen]
pub struct WasmPlayer {
    current_index: Option<usize>,
    player: PlayerImpl,
    playlist: Vec<String>,
}

#[wasm_bindgen]
impl WasmPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        info!("WasmPlayer created");
        Self {
            current_index: None,
            player: PlayerImpl::new(
                PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECONDS),
            ),
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
        self.current_index.map_or(-1, |idx| idx as i32)
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

    pub async fn select_track(&mut self, index: u32) -> Result<(), JsValue> {
        let idx = usize::try_from(index)
            .map_err(|_| js_error(format!("playlist index is too large: {index}")))?;
        let Some(url) = self.playlist.get(idx).cloned() else {
            return Err(js_error(format!("playlist index out of range: {idx}")));
        };

        let resource = load_resource(&url).await?;
        self.player
            .play_resource(resource)
            .map_err(|err| js_error(format!("failed to select track: {err}")))?;
        self.current_index = Some(idx);

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

    pub fn get_duration_ms(&self) -> f64 {
        self.player.duration_seconds().map_or(0.0, |s| s * 1000.0)
    }

    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
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
}

async fn load_resource(url: &str) -> Result<Resource, JsValue> {
    let config = ResourceConfig::new(url).map_err(|err| js_error(format!("invalid URL: {err}")))?;
    Resource::new(config)
        .await
        .map_err(|err| js_error(format!("failed to load resource: {err}")))
}
