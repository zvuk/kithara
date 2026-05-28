use std::sync::LazyLock;

use js_sys::Promise;
use wasm_bindgen::prelude::*;

use crate::player::AudioPlayer;

/// Process-wide [`AudioPlayer`] backing the deprecated `queue_*`
/// wasm-bindgen surface. Superseded by the `#[wasm_bindgen]` methods on
/// [`AudioPlayer`] itself (see [`crate::web::surface`]); these free
/// functions remain as thin shims until the JS demo migrates (Wave 6).
static PLAYER: LazyLock<AudioPlayer> = LazyLock::new(AudioPlayer::new_js);

fn player() -> &'static AudioPlayer {
    &PLAYER
}

fn resolved(value: impl Into<JsValue>) -> Promise {
    Promise::resolve(&value.into())
}

/// Boot the engine worker.
/// Deprecated: use `new AudioPlayer()`.
#[wasm_bindgen]
pub fn queue_new() -> Promise {
    let _ = player();
    Promise::resolve(&JsValue::UNDEFINED)
}

/// Append a track to the tail of the queue. Returns the allocated id.
///
/// # Errors
/// Returns a JS error if the queue rejects the item.
/// Deprecated: use `AudioPlayer.append`.
#[wasm_bindgen]
pub fn queue_append(url: String) -> Result<f64, JsValue> {
    player().append_js(url)
}

/// Insert a track after `after_id` (or at the head when negative).
///
/// # Errors
/// Returns a JS error if `after_id` is unknown.
/// Deprecated: use `AudioPlayer.insert`.
#[wasm_bindgen]
pub fn queue_insert(url: String, after_id: f64) -> Result<Promise, JsValue> {
    player().insert_js(url, after_id).map(resolved)
}

/// Remove a track by id.
///
/// # Errors
/// Returns a JS error if the id is unknown.
/// Deprecated: use `AudioPlayer.remove`.
#[wasm_bindgen]
pub fn queue_remove(id: f64) -> Result<Promise, JsValue> {
    player()
        .remove_js(id)
        .map(|()| resolved(JsValue::UNDEFINED))
}

/// Replace the track at `index`.
///
/// # Errors
/// Returns a JS error if `index` is out of range.
/// Deprecated: use `AudioPlayer.replaceItem`.
#[wasm_bindgen]
pub fn queue_replace(index: u32, url: String) -> Result<Promise, JsValue> {
    player().replace_item_js(index, url).map(resolved)
}

/// Select (start playing) a queued track with an immediate cut.
///
/// # Errors
/// Returns a JS error if `index` is out of range.
/// Deprecated: use `AudioPlayer.selectItem`.
#[wasm_bindgen]
pub fn queue_select(index: u32) -> Result<Promise, JsValue> {
    player()
        .select_item_js(index)
        .map(|()| resolved(JsValue::UNDEFINED))
}

/// Remove every track from the queue.
/// Deprecated: use `AudioPlayer.removeAllItems`.
#[wasm_bindgen]
pub fn queue_remove_all() {
    player().remove_all_items_js();
}

/// Start playback.
/// Deprecated: use `AudioPlayer.play`.
#[wasm_bindgen]
pub fn queue_play() {
    player().play_js();
}

/// Pause playback.
/// Deprecated: use `AudioPlayer.pause`.
#[wasm_bindgen]
pub fn queue_pause() {
    player().pause_js();
}

/// Pause and clear the queue.
/// Deprecated: use `AudioPlayer.stop`.
#[wasm_bindgen]
pub fn queue_stop() {
    player().stop_js();
}

/// Seek the current track to `position_ms`.
/// Deprecated: use `AudioPlayer.seek`.
#[wasm_bindgen]
pub fn queue_seek(position_ms: f64) {
    player().seek_js(position_ms);
}

/// Set the output volume (0.0..=1.0).
/// Deprecated: use `AudioPlayer.setVolume`.
#[wasm_bindgen]
pub fn queue_set_volume(volume: f32) {
    player().set_volume_js(volume);
}

/// Set the crossfade duration in seconds.
/// Deprecated: use `AudioPlayer.setCrossfadeSeconds`.
#[wasm_bindgen]
pub fn queue_set_crossfade_seconds(seconds: f32) {
    player().set_crossfade_seconds_js(seconds);
}

/// Set the gain for an EQ band.
///
/// # Errors
/// Returns a JS error if the engine rejects the change.
/// Deprecated: use `AudioPlayer.setEqGain`.
#[wasm_bindgen]
pub fn queue_set_eq_gain(band: u32, gain_db: f32) -> Result<(), JsValue> {
    player().set_eq_gain_js(band, gain_db)
}

/// Reset every EQ band to 0 dB.
///
/// # Errors
/// Returns a JS error if the engine rejects the change.
/// Deprecated: use `AudioPlayer.resetEq`.
#[wasm_bindgen]
pub fn queue_reset_eq() -> Result<(), JsValue> {
    player().reset_eq_js()
}
