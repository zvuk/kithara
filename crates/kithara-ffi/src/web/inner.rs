use std::sync::atomic::{AtomicU32, Ordering};

use js_sys::Function;
use kithara::{
    platform::sync::{Arc, Mutex},
    play::{CrossfadeSettings, DEFAULT_CROSSFADE_DURATION, InterruptionKind},
    queue::{ActionAtItemEnd, PlaybackOrder, RepeatMode, TrackId},
};

use crate::{
    item::AudioPlayerItem,
    observer::{FfiKeyProcessor, PlayerObserver, SeekCallback},
    types::{
        FfiAbrMode, FfiActionAtItemEnd, FfiCrossfadeSettings, FfiDuckingMode, FfiError, FfiKeyRule,
        FfiPlaybackOrder, FfiPlayerSnapshot, FfiPlayerStatus, FfiRepeatMode,
    },
    web::{bridge::WorkerBridge, commands::WorkerCmd, observer::router::Routes},
};

/// Number of EQ bands surfaced through the wasm facade. Module-level
/// because the struct's `eq_gains` array length references it (a position
/// where `Self::` is not yet in scope); the remaining scalar defaults
/// live as `WasmInner` associated consts.
const EQ_BANDS: usize = 10;

/// Caller-facing ordered queue view: the `(TrackId, item)` pairs the
/// caller inserted, in queue order. The worker owns the canonical
/// [`Queue`](kithara::queue::Queue); this mirror exists because the caller
/// allocates the [`TrackId`](kithara::queue::TrackId) on the main thread
/// and the worker plants the identical id via `*_with_id`, so order is
/// deterministic without a round-trip. Drives `items` / `item_count`
/// exactly as `NativeInner`'s registry + `queue.tracks()` order do on
/// native.
type QueueView = Vec<(TrackId, Arc<AudioPlayerItem>)>;

/// Wasm implementation of the FFI player engine, parallel to
/// [`NativeInner`](crate::native::inner::NativeInner). Exposes the same
/// inherent method set the [`AudioPlayer`](crate::player) facade delegates
/// to, so the single facade body type-checks on both targets.
///
/// The worker owns a canonical Host member and its queue control; `WasmInner`
/// owns the command channel into it plus the main-thread caller-facing state
/// (cached scalar settings + the ordered queue view). Setters write through to
/// both the worker and the local cache so the infallible facade getters can
/// answer synchronously without a worker round-trip.
pub(crate) struct WasmInner {
    queue_view: Arc<Mutex<QueueView>>,
    playing_rate: AtomicU32,
    volume: AtomicU32,
    action_at_item_end: Mutex<FfiActionAtItemEnd>,
    crossfade_settings: Mutex<FfiCrossfadeSettings>,
    muted: Mutex<bool>,
    playback_order: Mutex<FfiPlaybackOrder>,
    repeat_mode: Mutex<FfiRepeatMode>,
    routes: Routes,
    bridge: WorkerBridge,
    eq_gains: [AtomicU32; EQ_BANDS],
}

impl Default for WasmInner {
    fn default() -> Self {
        let queue_view: Arc<Mutex<QueueView>> = Arc::new(Mutex::default());
        Self {
            bridge: WorkerBridge::default(),
            routes: Routes::new(Arc::clone(&queue_view)),
            queue_view,
            volume: AtomicU32::new(Self::DEFAULT_VOLUME.to_bits()),
            crossfade_settings: Mutex::new(FfiCrossfadeSettings {
                duration: Self::DEFAULT_CROSSFADE_SECONDS,
                curve: crate::types::FfiCrossfadeCurve::EqualPower,
                depth: 1.0,
                position: 0.5,
            }),
            playing_rate: AtomicU32::new(Self::DEFAULT_PLAYING_RATE.to_bits()),
            repeat_mode: Mutex::new(FfiRepeatMode::Off),
            playback_order: Mutex::new(FfiPlaybackOrder::Sequential),
            action_at_item_end: Mutex::new(FfiActionAtItemEnd::Advance),
            muted: Mutex::default(),
            eq_gains: [const { AtomicU32::new(0) }; EQ_BANDS],
        }
    }
}

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl WasmInner {
    /// Default crossfade window in seconds, matching the worker default.
    const DEFAULT_CROSSFADE_SECONDS: f32 = DEFAULT_CROSSFADE_DURATION;
    /// Default target playback rate.
    const DEFAULT_PLAYING_RATE: f32 = 1.0;
    /// Default output volume, matching the legacy wasm player.
    const DEFAULT_VOLUME: f32 = 0.5;
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    pub(crate) fn action_at_item_end(&self) -> FfiActionAtItemEnd {
        *self.action_at_item_end.lock()
    }

    pub(crate) fn advance_to_next_item(&self) -> Result<(), FfiError> {
        self.try_send(WorkerCmd::Next)
    }

    /// Start (or restart) the analysis pass for a queued track.
    pub(crate) fn analyze(&self, id: TrackId) -> Result<(), FfiError> {
        let request_id = Self::next_request_id();
        self.try_send(WorkerCmd::Analyze { id, request_id })
    }

    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let id = item.track_id();
        self.try_send(WorkerCmd::Append {
            id,
            url: item.url(),
        })?;
        *item.inserted.lock() = true;
        self.queue_view.lock().push((id, Arc::clone(item)));
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn crossfade_settings(&self) -> FfiCrossfadeSettings {
        *self.crossfade_settings.lock()
    }
    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let current = self.bridge.current_track_id()?;
        self.queue_view
            .lock()
            .iter()
            .find(|(id, _)| *id == current)
            .map(|(_, item)| Arc::clone(item))
    }
    pub(crate) fn eq_band_count(&self) -> u32 {
        let n = self.eq_gains.len();
        u32::try_from(n).unwrap_or_else(|_| {
            tracing::error!(eq_band_count = n, "BUG: EQ band count exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn eq_gain(&self, band: u32) -> f32 {
        self.eq_gains.get(band as usize).map_or(0.0, load_f32)
    }

    pub(crate) fn insert(
        &self,
        item: &Arc<AudioPlayerItem>,
        after: Option<&Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let id = item.track_id();
        let after_id = after.map(|i| i.track_id());
        let request_id = Self::next_request_id();
        self.send(WorkerCmd::Insert {
            id,
            request_id,
            url: item.url(),
            after: after_id,
        });

        let mut view = self.queue_view.lock();
        let pos = match after_id {
            None => 0,
            Some(after_id) => view
                .iter()
                .position(|(existing, _)| *existing == after_id)
                .map(|i| i + 1)
                .ok_or_else(|| FfiError::InvalidArgument {
                    reason: format!("after id {after_id:?} not in queue"),
                })?,
        };
        view.insert(pos, (id, Arc::clone(item)));
        drop(view);

        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn is_muted(&self) -> bool {
        *self.muted.lock()
    }

    pub(crate) fn item_count(&self) -> u32 {
        let len = self.queue_view.lock().len();
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.queue_view
            .lock()
            .iter()
            .map(|(_, item)| Arc::clone(item))
            .collect()
    }

    fn next_request_id() -> u32 {
        crate::web::interop::next_request_id()
    }

    pub(crate) fn notify_audio_route_changed(&self, _reason: &str) -> Result<(), FfiError> {
        Ok(())
    }

    pub(crate) fn notify_interruption(&self, _kind: InterruptionKind) {}

    pub(crate) fn pause(&self) {
        self.send(WorkerCmd::Pause);
    }

    pub(crate) fn play(&self) {
        self.send(WorkerCmd::Play);
    }

    pub(crate) fn playback_order(&self) -> FfiPlaybackOrder {
        *self.playback_order.lock()
    }

    pub(crate) fn playing_rate(&self) -> f32 {
        load_f32(&self.playing_rate)
    }

    pub(crate) fn rate(&self) -> f32 {
        if self.bridge.is_playing() {
            load_f32(&self.playing_rate)
        } else {
            0.0
        }
    }

    pub(crate) fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        if !*item.inserted.lock() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item {} not in queue", item.audio_id()),
            });
        }
        let id = item.track_id();
        let request_id = Self::next_request_id();
        self.send(WorkerCmd::Remove { id, request_id });
        self.queue_view
            .lock()
            .retain(|(existing, _)| *existing != id);
        *item.inserted.lock() = false;
        Ok(())
    }

    pub(crate) fn remove_all_items(&self) {
        self.send(WorkerCmd::RemoveAll);
        let mut view = self.queue_view.lock();
        for (_, item) in view.drain(..) {
            *item.inserted.lock() = false;
        }
    }

    pub(crate) fn repeat_mode(&self) -> FfiRepeatMode {
        *self.repeat_mode.lock()
    }

    pub(crate) fn replace_item(
        &self,
        index: u32,
        item: &Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let idx = index as usize;
        let new_id = item.track_id();
        let request_id = Self::next_request_id();

        let mut view = self.queue_view.lock();
        if idx >= view.len() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item index {idx} out of range (len: {})", view.len()),
            });
        }
        self.send(WorkerCmd::Replace {
            index,
            request_id,
            id: new_id,
            url: item.url(),
        });
        if let Some((_, old)) = view.get(idx) {
            *old.inserted.lock() = false;
        }
        view[idx] = (new_id, Arc::clone(item));
        drop(view);

        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn reset_eq(&self) -> Result<(), FfiError> {
        for g in &self.eq_gains {
            store_f32(g, 0.0);
        }
        self.try_send(WorkerCmd::ResetEq)
    }

    pub(crate) fn return_to_previous_item(&self) -> Result<(), FfiError> {
        self.try_send(WorkerCmd::Previous)
    }

    pub(crate) fn seek(
        &self,
        to_seconds: f64,
        tolerance: Option<f64>,
        callback: &Arc<dyn SeekCallback>,
    ) {
        let _ = tolerance;
        self.send(WorkerCmd::Seek(to_seconds.max(0.0) * Self::MS_PER_SECOND));
        callback.on_complete(true);
    }

    /// Fire-and-forget seek in milliseconds for the JS control surface
    /// (no [`SeekCallback`] round-trip). The shared facade `seek` carries
    /// a callback; this is the wasm-only convenience the JS surface uses.
    pub(crate) fn seek_ms(&self, position_ms: f64) {
        self.send(WorkerCmd::Seek(position_ms.max(0.0)));
    }

    pub(crate) fn select(
        &self,
        item: &AudioPlayerItem,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        if !*item.inserted.lock() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item {} not in queue", item.audio_id()),
            });
        }
        let request_id = Self::next_request_id();
        self.send(WorkerCmd::SelectQueue {
            request_id,
            id: item.track_id(),
            transition: transition.try_into()?,
        });
        Ok(())
    }

    /// Forward a command for the infallible facade methods (play / pause /
    /// volume / …), logging a dropped command rather than propagating —
    /// these have no error channel in the facade signature.
    fn send(&self, cmd: WorkerCmd) {
        if let Err(err) = self.bridge.send(cmd) {
            tracing::warn!(?err, "wasm worker command dropped: channel unavailable");
        }
    }

    pub(crate) fn set_abr_mode(&self, mode: FfiAbrMode) {
        let variant_index = match mode {
            FfiAbrMode::Auto => None,
            FfiAbrMode::Manual { variant_index } => Some(variant_index),
        };
        self.send(WorkerCmd::SetAbrMode { variant_index });
    }

    pub(crate) fn set_action_at_item_end(
        &self,
        action: FfiActionAtItemEnd,
    ) -> Result<(), FfiError> {
        let typed: ActionAtItemEnd = action.try_into()?;
        self.try_send(WorkerCmd::SetActionAtItemEnd(typed))?;
        *self.action_at_item_end.lock() = action;
        Ok(())
    }

    pub(crate) fn set_crossfade_settings(
        &self,
        settings: FfiCrossfadeSettings,
    ) -> Result<(), FfiError> {
        let typed: CrossfadeSettings = settings.try_into()?;
        self.try_send(WorkerCmd::SetCrossfade(typed))?;
        *self.crossfade_settings.lock() = settings;
        Ok(())
    }

    pub(crate) fn set_ducking_mode(&self, mode: FfiDuckingMode) -> Result<(), FfiError> {
        self.try_send(WorkerCmd::SetDucking(mode.into()))
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            store_f32(slot, gain_db);
        }
        self.try_send(WorkerCmd::SetEqGain { band, gain_db })
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        *self.muted.lock() = muted;
        let volume = if muted { 0.0 } else { load_f32(&self.volume) };
        self.send(WorkerCmd::SetVolume(volume));
    }

    pub(crate) fn set_playback_order(&self, order: FfiPlaybackOrder) -> Result<(), FfiError> {
        let typed: PlaybackOrder = order.try_into()?;
        self.try_send(WorkerCmd::SetPlaybackOrder(typed))?;
        *self.playback_order.lock() = order;
        Ok(())
    }

    pub(crate) fn set_playing_rate(&self, rate: f32) {
        store_f32(&self.playing_rate, rate);
    }

    pub(crate) fn set_repeat_mode(&self, mode: FfiRepeatMode) -> Result<(), FfiError> {
        let mode = RepeatMode::try_from(mode).map_err(|rejected| FfiError::InvalidArgument {
            reason: format!("repeat mode {rejected:?} is not supported"),
        })?;
        self.try_send(WorkerCmd::SetRepeat(mode))?;
        *self.repeat_mode.lock() = mode.into();
        Ok(())
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        store_f32(&self.volume, volume);
        if !*self.muted.lock() {
            self.send(WorkerCmd::SetVolume(volume));
        }
    }

    pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = crate::web::interop::generate_salt();
        let rule = FfiKeyRule {
            processor,
            headers: None,
            query_params: None,
            domains: vec!["*".to_string()],
            salt: Some(salt),
        };
        self.setup_hls_aes_with_rule(rule);
    }

    pub(crate) fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule) {
        crate::web::keys::install_main_processor(Arc::clone(&rule.processor));
        let salt = rule.salt.unwrap_or_else(crate::web::interop::generate_salt);
        self.send(WorkerCmd::SetupHlsAes {
            salt,
            domains: rule.domains,
            headers: rule.headers,
            query_params: rule.query_params,
        });
    }

    pub(crate) fn setup_network(&self, auth_token: String) {
        self.send(WorkerCmd::AuthToken { token: auth_token });
    }

    pub(crate) fn snapshot(&self) -> FfiPlayerSnapshot {
        let position = self.bridge.position_secs();
        let duration = self.bridge.duration_secs();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::ReadyToPlay,
            current_time: (position > 0.0).then_some(position),
            duration: (duration > 0.0).then_some(duration),
            rate: self.rate(),
            playing_rate: load_f32(&self.playing_rate),
            volume: load_f32(&self.volume),
            is_muted: *self.muted.lock(),
        }
    }

    pub(crate) fn stop(&self) {
        self.send(WorkerCmd::Stop);
    }

    pub(crate) fn update_peak_bitrate(&self, wifi_bps: f64, cellular_bps: f64) {
        self.send(WorkerCmd::PeakBitrate {
            wifi_bps,
            cellular_bps,
        });
    }

    pub(crate) fn volume(&self) -> f32 {
        load_f32(&self.volume)
    }

    delegate::delegate! {
        to self.routes {
            #[call(set_analysis)]
            pub(crate) fn set_analysis_observer(&self, func: Function);
            #[call(set_player)]
            pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>);
        }
        to self.bridge {
            #[call(position_secs)]
            pub(crate) fn current_time(&self) -> f64;
            /// Audio-thread process calls served so far.
            #[call(process_calls)]
            pub(crate) fn rt_process_calls(&self) -> u64;
            /// Underruns the audio thread has recorded so far.
            #[call(underruns)]
            pub(crate) fn rt_underruns(&self) -> u64;
            /// Forward a command to the worker, mapping a channel failure to a
            /// typed [`FfiError`]. Used by the fallible facade methods that should
            /// surface a real error when the worker link is down.
            #[expr($.map_err(|err| into_internal(&err)))]
            #[call(send)]
            fn try_send(&self, cmd: WorkerCmd) -> Result<(), FfiError>;
        }
    }
}

/// Wasm `Inner` alias consumed by the cross-platform
/// [`AudioPlayer`](crate::player) facade. Parallel to
/// [`NativeInner`](crate::native::inner::NativeInner) on native.
pub(crate) type Inner = WasmInner;

fn into_internal(err: &wasm_bindgen::JsValue) -> FfiError {
    FfiError::Internal {
        description: err
            .as_string()
            .unwrap_or_else(|| "wasm worker error".into()),
    }
}
