use js_sys::{Function, Object, Reflect};
use kithara::platform::sync::Arc;
use num_traits::cast;
use wasm_bindgen::prelude::*;

use crate::{
    item::AudioPlayerItem,
    player::AudioPlayer,
    types::{
        FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeCurve, FfiCrossfadeSettings, FfiItemConfig,
        FfiPlaybackOrder, FfiTransition,
    },
    web::observer::shim::{ItemObserverJs, KeyProcessorJs, PlayerObserverJs, SeekCallbackJs},
};

mod consts {
    /// Milliseconds per second.
    pub(super) const MS_PER_SECOND: f64 = 1000.0;
}

fn item_for_url(url: String) -> Arc<AudioPlayerItem> {
    AudioPlayerItem::new(FfiItemConfig {
        url,
        abr_mode: None,
        headers: None,
        audio_id: None,
        uuid_i64: None,
        is_live_stream: false,
        preferred_peak_bitrate: 0.0,
        preferred_peak_bitrate_expensive: 0.0,
    })
}

fn id_to_f64(item: &Arc<AudioPlayerItem>) -> f64 {
    cast::<u64, f64>(item.track_id().as_u64()).unwrap_or(0.0)
}

/// Browser control surface for the cross-platform
/// [`AudioPlayer`](crate::player::AudioPlayer) facade. Wraps the facade's
/// `Inner` (the wasm engine) and adapts JS argument shapes (URLs, `f64`
/// track ids, JS callback objects) onto the typed facade methods.
#[wasm_bindgen]
impl AudioPlayer {
    #[wasm_bindgen(js_name = actionAtItemEnd)]
    pub fn action_at_item_end_js(&self) -> String {
        match self.inner.action_at_item_end() {
            FfiActionAtItemEnd::Advance => "advance",
            FfiActionAtItemEnd::Pause => "pause",
            FfiActionAtItemEnd::None => "none",
            FfiActionAtItemEnd::Unknown => "unknown",
        }
        .to_owned()
    }

    /// Subscribe `obj` to the marshalled
    /// [`FfiItemEvent`](crate::types::FfiItemEvent) objects of the track
    /// with `id`. Returns the observer handle `removeItemObserver` takes.
    ///
    /// # Errors
    /// Returns a JS error if the id is not in the queue or `obj` is not a
    /// callable function.
    #[wasm_bindgen(js_name = addItemObserver)]
    pub fn add_item_observer_js(&self, id: f64, obj: JsValue) -> Result<f64, JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("observer must be a function"))?;
        let item = self
            .item_by_id(id)
            .ok_or_else(|| JsValue::from_str("unknown track id"))?;
        let observer_id = item.add_observer(Arc::new(ItemObserverJs::new(func)));
        Ok(cast::<u64, f64>(observer_id).unwrap_or(0.0))
    }

    /// Start (or restart) the analysis pass for a queued track.
    ///
    /// # Errors
    /// Returns a JS error if the id is not in the queue or analysis is unavailable.
    #[wasm_bindgen(js_name = analyze)]
    pub fn analyze_js(&self, track_id: f64) -> Result<(), JsValue> {
        let item = self
            .item_by_id(track_id)
            .ok_or_else(|| JsValue::from_str("unknown track id"))?;
        self.inner
            .analyze(item.track_id())
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

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

    #[wasm_bindgen(js_name = crossfadeSettings)]
    pub fn crossfade_settings_js(&self) -> Result<Object, JsValue> {
        let settings = self.inner.crossfade_settings();
        let object = Object::new();
        Reflect::set(&object, &"duration".into(), &settings.duration.into())?;
        Reflect::set(
            &object,
            &"curve".into(),
            &match settings.curve {
                FfiCrossfadeCurve::Linear => "linear",
                FfiCrossfadeCurve::EqualPower => "equalPower",
                FfiCrossfadeCurve::Unknown => "unknown",
            }
            .into(),
        )?;
        Reflect::set(&object, &"depth".into(), &settings.depth.into())?;
        Reflect::set(&object, &"position".into(), &settings.position.into())?;
        Ok(object)
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
        self.inner.current_time() * consts::MS_PER_SECOND
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

    pub(crate) fn item_by_id(&self, raw: f64) -> Option<Arc<AudioPlayerItem>> {
        if raw < 0.0 {
            return None;
        }
        let id: u64 = cast(raw).unwrap_or(0);
        self.inner
            .items()
            .into_iter()
            .find(|item| item.track_id().as_u64() == id)
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
            inner: crate::Inner::default(),
        }
    }

    #[wasm_bindgen(js_name = next)]
    pub fn next_js(&self) -> Result<(), JsValue> {
        self.inner
            .advance_to_next_item()
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    #[wasm_bindgen(js_name = pause)]
    pub fn pause_js(&self) {
        self.inner.pause();
    }

    #[wasm_bindgen(js_name = play)]
    pub fn play_js(&self) {
        self.inner.play();
    }

    #[wasm_bindgen(js_name = playbackOrder)]
    pub fn playback_order_js(&self) -> String {
        match self.inner.playback_order() {
            FfiPlaybackOrder::Sequential => "sequential",
            FfiPlaybackOrder::Shuffle => "shuffle",
            FfiPlaybackOrder::Unknown => "unknown",
        }
        .to_owned()
    }

    #[wasm_bindgen(js_name = previous)]
    pub fn previous_js(&self) -> Result<(), JsValue> {
        self.inner
            .return_to_previous_item()
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    #[wasm_bindgen(js_name = removeAllItems)]
    pub fn remove_all_items_js(&self) {
        self.inner.remove_all_items();
    }

    /// Unsubscribe the observer registered under `observer_id` from the
    /// track with `id`.
    ///
    /// # Errors
    /// Returns a JS error if the id is not in the queue.
    #[wasm_bindgen(js_name = removeItemObserver)]
    pub fn remove_item_observer_js(&self, id: f64, observer_id: f64) -> Result<(), JsValue> {
        let item = self
            .item_by_id(id)
            .ok_or_else(|| JsValue::from_str("unknown track id"))?;
        item.remove_observer(cast(observer_id).unwrap_or(u64::MAX));
        Ok(())
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

    /// Audio-thread process calls served so far.
    ///
    /// The render callback runs inside an `AudioWorkletProcessor`, which the
    /// page cannot observe: a browser that stops calling it leaves the
    /// session reporting itself as playing while the position stands still.
    /// The counter is monotonic, so a caller reads it twice and compares:
    /// no growth means the callback is no longer running. Saturates at
    /// `u32::MAX`, which a browser session reaches after months of playback.
    #[wasm_bindgen(js_name = rtProcessCalls)]
    #[must_use]
    pub fn rt_process_calls_js(&self) -> u32 {
        u32::try_from(self.inner.rt_process_calls()).unwrap_or(u32::MAX)
    }

    /// Underruns the audio thread has recorded so far. Read next to
    /// [`Self::rt_process_calls_js`]: a callback that runs while this climbs
    /// is starving rather than stopped. Saturates like
    /// [`Self::rt_process_calls_js`].
    #[wasm_bindgen(js_name = rtUnderruns)]
    #[must_use]
    pub fn rt_underruns_js(&self) -> u32 {
        u32::try_from(self.inner.rt_underruns()).unwrap_or(u32::MAX)
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
            .seek(position_ms / consts::MS_PER_SECOND, None, &callback);
        Ok(())
    }

    /// Select (start playing) the track with `id` with an immediate cut.
    ///
    /// # Errors
    /// Returns a JS error if the id is not in the queue.
    #[wasm_bindgen(js_name = select)]
    pub fn select_js(&self, id: f64) -> Result<(), JsValue> {
        let item = self
            .item_by_id(id)
            .ok_or_else(|| JsValue::from_str("unknown track id"))?;
        self.inner
            .select(&item, FfiTransition::None)
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

    #[wasm_bindgen(js_name = setActionAtItemEnd)]
    pub fn set_action_at_item_end_js(&self, action: String) -> Result<(), JsValue> {
        let action = match action.as_str() {
            "advance" => FfiActionAtItemEnd::Advance,
            "pause" => FfiActionAtItemEnd::Pause,
            "none" => FfiActionAtItemEnd::None,
            _ => return Err(JsValue::from_str("invalid terminal action")),
        };
        self.inner
            .set_action_at_item_end(action)
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Register a JS callback receiving analysis publications.
    ///
    /// # Errors
    /// Returns a JS error if `obj` is not a callable function.
    #[wasm_bindgen(js_name = setAnalysisObserver)]
    pub fn set_analysis_observer_js(&self, obj: JsValue) -> Result<(), JsValue> {
        let func: Function = obj
            .dyn_into()
            .map_err(|_| JsValue::from_str("observer must be a function"))?;
        self.inner.set_analysis_observer(func);
        Ok(())
    }

    #[wasm_bindgen(js_name = setCrossfadeSettings)]
    pub fn set_crossfade_settings_js(
        &self,
        duration: f32,
        curve: String,
        depth: f32,
        position: f32,
    ) -> Result<(), JsValue> {
        let curve = match curve.as_str() {
            "linear" => FfiCrossfadeCurve::Linear,
            "equalPower" => FfiCrossfadeCurve::EqualPower,
            _ => return Err(JsValue::from_str("invalid crossfade curve")),
        };
        self.inner
            .set_crossfade_settings(FfiCrossfadeSettings {
                duration,
                curve,
                depth,
                position,
            })
            .map_err(|error| JsValue::from_str(&error.to_string()))
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

    #[wasm_bindgen(js_name = setPlaybackOrder)]
    pub fn set_playback_order_js(&self, order: String) -> Result<(), JsValue> {
        let order = match order.as_str() {
            "sequential" => FfiPlaybackOrder::Sequential,
            "shuffle" => FfiPlaybackOrder::Shuffle,
            _ => return Err(JsValue::from_str("invalid playback order")),
        };
        self.inner
            .set_playback_order(order)
            .map_err(|error| JsValue::from_str(&error.to_string()))
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
        crate::web::bridge::tick_and_poll();
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
