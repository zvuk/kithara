use std::sync::Arc;

use js_sys::Function;
use kithara_play::wasm_support::tick_and_poll;
use num_traits::cast;
use wasm_bindgen::prelude::*;

use crate::{
    item::AudioPlayerItem,
    player::AudioPlayer,
    types::{FfiAbrMode, FfiItemConfig, FfiPlayerConfig, FfiTransition},
    web::observer::shim::{ItemObserverJs, KeyProcessorJs, PlayerObserverJs, SeekCallbackJs},
};

/// Milliseconds per second.
const MS_PER_SECOND: f64 = 1000.0;

fn item_for_url(url: String) -> Arc<AudioPlayerItem> {
    AudioPlayerItem::new(FfiItemConfig {
        url,
        abr_mode: None,
        headers: None,
        is_live_stream: false,
        preferred_peak_bitrate: 0.0,
        preferred_peak_bitrate_expensive: 0.0,
    })
}

fn id_to_f64(item: &Arc<AudioPlayerItem>) -> f64 {
    cast::<u64, f64>(item.audio_id().as_u64()).unwrap_or(0.0)
}

/// Browser control surface for the cross-platform
/// [`AudioPlayer`](crate::player::AudioPlayer) facade. Wraps the facade's
/// `Inner` (the wasm engine) and adapts JS argument shapes (URLs, `f64`
/// track ids, JS callback objects) onto the typed facade methods.
#[wasm_bindgen]
impl AudioPlayer {
    /// Append a track to the tail of the queue. Returns the allocated
    /// track id as an `f64`.
    ///
    /// # Errors
    /// Returns a JS error if the queue rejects the item.
    #[wasm_bindgen(js_name = append)]
    pub fn append_js(&self, url: String) -> Result<f64, JsValue> {
        let item = item_for_url(url);
        self.inner
            .append(&item)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(id_to_f64(&item))
    }

    #[wasm_bindgen(js_name = crossfadeSeconds)]
    #[must_use]
    pub fn crossfade_seconds_js(&self) -> f32 {
        self.inner.crossfade_duration()
    }

    /// Track id (`f64`) of the currently playing item, or `-1.0` if none.
    #[wasm_bindgen(js_name = currentItemId)]
    #[must_use]
    pub fn current_item_id_js(&self) -> f64 {
        self.inner
            .current_item()
            .map_or(-1.0, |item| id_to_f64(&item))
    }

    #[wasm_bindgen(js_name = currentTimeMs)]
    #[must_use]
    pub fn current_time_ms_js(&self) -> f64 {
        self.inner.current_time() * MS_PER_SECOND
    }

    #[wasm_bindgen(js_name = eqBandCount)]
    #[must_use]
    pub fn eq_band_count_js(&self) -> u32 {
        self.inner.eq_band_count()
    }

    #[wasm_bindgen(js_name = eqGain)]
    #[must_use]
    pub fn eq_gain_js(&self, band: u32) -> f32 {
        self.inner.eq_gain(band)
    }

    /// Insert a track after the item with `after_id` (or at the head when
    /// `after_id` is negative). Returns the new track id.
    ///
    /// # Errors
    /// Returns a JS error if `after_id` is unknown.
    #[wasm_bindgen(js_name = insert)]
    pub fn insert_js(&self, url: String, after_id: f64) -> Result<f64, JsValue> {
        let item = item_for_url(url);
        let after = self.item_by_id(after_id);
        self.inner
            .insert(&item, after.as_ref())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(id_to_f64(&item))
    }

    #[wasm_bindgen(js_name = isMuted)]
    #[must_use]
    pub fn is_muted_js(&self) -> bool {
        self.inner.is_muted()
    }

    fn item_by_id(&self, raw: f64) -> Option<Arc<AudioPlayerItem>> {
        if raw < 0.0 {
            return None;
        }
        let id: u64 = cast(raw).unwrap_or(0);
        self.inner
            .items()
            .into_iter()
            .find(|item| item.audio_id().as_u64() == id)
    }

    #[wasm_bindgen(js_name = itemCount)]
    #[must_use]
    pub fn item_count_js(&self) -> u32 {
        self.inner.item_count()
    }

    /// Construct a player. The engine worker and the audio worklet boot
    /// lazily on the first command (via the worker bridge), not here: both
    /// grow the shared `SharedArrayBuffer` concurrently with the main
    /// thread, and wasm-bindgen boxes this handle's `Rc` *after* the
    /// constructor body runs. Booting eagerly would box the handle in the
    /// middle of that concurrent-grow storm, landing it above the main
    /// instance's visible memory bound and trapping every later
    /// `__wbg_ptr` deref with "memory access out of bounds". Constructing
    /// single-threaded keeps the handle in the main thread's own grown
    /// region, always addressable.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new_js() -> Self {
        Self {
            inner: crate::Inner::new(FfiPlayerConfig::default()),
        }
    }

    #[wasm_bindgen(js_name = pause)]
    pub fn pause_js(&self) {
        self.inner.pause();
    }

    #[wasm_bindgen(js_name = play)]
    pub fn play_js(&self) {
        self.inner.play();
    }

    #[wasm_bindgen(js_name = removeAllItems)]
    pub fn remove_all_items_js(&self) {
        self.inner.remove_all_items();
    }

    /// Remove a track by id.
    ///
    /// # Errors
    /// Returns a JS error if the id is not in the queue.
    #[wasm_bindgen(js_name = remove)]
    pub fn remove_js(&self, id: f64) -> Result<(), JsValue> {
        let item = self
            .item_by_id(id)
            .ok_or_else(|| JsValue::from_str("unknown track id"))?;
        self.inner
            .remove(&item)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Replace the track at `index`. Returns the new track id.
    ///
    /// # Errors
    /// Returns a JS error if `index` is out of range.
    #[wasm_bindgen(js_name = replaceItem)]
    pub fn replace_item_js(&self, index: u32, url: String) -> Result<f64, JsValue> {
        let item = item_for_url(url);
        self.inner
            .replace_item(index, &item)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(id_to_f64(&item))
    }

    /// Reset every EQ band to 0 dB.
    ///
    /// # Errors
    /// Returns a JS error if the engine rejects the change.
    #[wasm_bindgen(js_name = resetEq)]
    pub fn reset_eq_js(&self) -> Result<(), JsValue> {
        self.inner
            .reset_eq()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = seek)]
    pub fn seek_js(&self, position_ms: f64) {
        self.inner.seek_ms(position_ms);
    }

    /// Seek to `position_ms` and invoke the JS callback `obj` with a
    /// boolean once the seek command is accepted. Adapts the JS function
    /// into the typed `Arc<dyn SeekCallback>` the shared facade `seek`
    /// expects.
    ///
    /// # Errors
    /// Returns a JS error if `obj` is not a callable function.
    #[wasm_bindgen(js_name = seekWithCallback)]
    pub fn seek_with_callback_js(&self, position_ms: f64, obj: JsValue) -> Result<(), JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("callback must be a function"))?;
        let callback: Arc<dyn crate::observer::SeekCallback> = Arc::new(SeekCallbackJs::new(func));
        self.inner
            .seek(position_ms / MS_PER_SECOND, None, &callback);
        Ok(())
    }

    /// Select (start playing) the track at `index` with an immediate cut.
    ///
    /// # Errors
    /// Returns a JS error if `index` is out of range.
    #[wasm_bindgen(js_name = selectItem)]
    pub fn select_item_js(&self, index: u32) -> Result<(), JsValue> {
        self.inner
            .select_item(index, FfiTransition::None)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Pin a manual ABR variant by index, or pass a negative index to
    /// restore automatic adaptation.
    #[wasm_bindgen(js_name = setAbrMode)]
    pub fn set_abr_mode_js(&self, variant_index: i32) {
        let mode = if variant_index < 0 {
            FfiAbrMode::Auto
        } else {
            FfiAbrMode::Manual {
                variant_index: cast(variant_index).unwrap_or(0),
            }
        };
        self.inner.set_abr_mode(mode);
    }

    #[wasm_bindgen(js_name = setCrossfadeSeconds)]
    pub fn set_crossfade_seconds_js(&self, seconds: f32) {
        self.inner.set_crossfade_duration(seconds);
    }

    /// Set the gain (dB) for an EQ band.
    ///
    /// # Errors
    /// Returns a JS error if the engine rejects the change.
    #[wasm_bindgen(js_name = setEqGain)]
    pub fn set_eq_gain_js(&self, band: u32, gain_db: f32) -> Result<(), JsValue> {
        self.inner
            .set_eq_gain(band, gain_db)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Register a JS callback as the per-item observer for the track at
    /// `index`. The callback receives marshalled
    /// [`FfiItemEvent`](crate::types::FfiItemEvent) objects.
    ///
    /// # Errors
    /// Returns a JS error if `index` is out of range or `obj` is not a
    /// callable function.
    #[wasm_bindgen(js_name = setItemObserver)]
    pub fn set_item_observer_js(&self, index: u32, obj: JsValue) -> Result<(), JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("observer must be a function"))?;
        let item = self
            .inner
            .items()
            .into_iter()
            .nth(index as usize)
            .ok_or_else(|| JsValue::from_str("item index out of range"))?;
        item.set_observer(Arc::new(ItemObserverJs::new(func)));
        Ok(())
    }

    #[wasm_bindgen(js_name = setMuted)]
    pub fn set_muted_js(&self, muted: bool) {
        self.inner.set_muted(muted);
    }

    /// Register a JS callback (`obj`) as the player-level observer. The
    /// callback receives one marshalled event object per
    /// [`FfiPlayerEvent`](crate::types::FfiPlayerEvent). This is one of
    /// the FFI boundary sites: a JS `Function` is adapted into the typed
    /// `Arc<dyn PlayerObserver>` the facade expects.
    ///
    /// # Errors
    /// Returns a JS error if `obj` is not a callable function.
    #[wasm_bindgen(js_name = setObserver)]
    pub fn set_observer_js(&self, obj: JsValue) -> Result<(), JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("observer must be a function"))?;
        self.inner
            .set_observer(Arc::new(PlayerObserverJs::new(func)));
        Ok(())
    }

    #[wasm_bindgen(js_name = setVolume)]
    pub fn set_volume_js(&self, volume: f32) {
        self.inner.set_volume(volume);
    }

    /// Register a JS DRM key processor (`process_key(key: Uint8Array,
    /// salt: string) -> Uint8Array`) and arm a wildcard AES key rule. This
    /// is the FFI boundary site that adapts the JS `Function` into the
    /// typed `Arc<dyn FfiKeyProcessor>` the facade expects; the worker then
    /// routes every segment-key decrypt back to this callback over the
    /// cross-thread key bridge.
    ///
    /// # Errors
    /// Returns a JS error if `obj` is not a callable function.
    #[wasm_bindgen(js_name = setupHlsAes)]
    pub fn setup_hls_aes_js(&self, obj: JsValue) -> Result<(), JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("key processor must be a function"))?;
        self.inner
            .setup_hls_aes(Arc::new(KeyProcessorJs::new(func)));
        Ok(())
    }

    /// Set or clear the player-wide auth token (`X-Auth-Token`). An empty
    /// string clears it. Applied to subsequently built tracks.
    #[wasm_bindgen(js_name = setupNetwork)]
    pub fn setup_network_js(&self, auth_token: String) {
        self.inner.setup_network(auth_token);
    }

    #[wasm_bindgen(js_name = stop)]
    pub fn stop_js(&self) {
        self.inner.stop();
    }

    /// Drive the main-thread pumps once. Call from a
    /// `requestAnimationFrame` loop: it polls the worker → main session
    /// channel (audio-graph updates) and services pending DRM key requests
    /// (invoking the registered JS key callback). Mirrors the legacy
    /// `player_tick` for the unified facade.
    #[wasm_bindgen(js_name = tick)]
    pub fn tick_js(&self) {
        tick_and_poll();
        crate::web::key_processor_bridge::pump();
    }

    /// Cap ABR variant selection by per-network peak bitrate (bits/sec).
    /// `0.0` lifts the cap for that network.
    #[wasm_bindgen(js_name = updatePeakBitrate)]
    pub fn update_peak_bitrate_js(&self, wifi_bps: f64, cellular_bps: f64) {
        self.inner.update_peak_bitrate(wifi_bps, cellular_bps);
    }

    #[wasm_bindgen(js_name = volume)]
    #[must_use]
    pub fn volume_js(&self) -> f32 {
        self.inner.volume()
    }
}
