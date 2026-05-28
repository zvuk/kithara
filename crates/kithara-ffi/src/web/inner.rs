use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use kithara_platform::Mutex;
use kithara_queue::Transition;

use crate::{
    item::AudioPlayerItem,
    observer::{FfiKeyProcessor, PlayerObserver, SeekCallback},
    types::{
        FfiAbrMode, FfiError, FfiKeyRule, FfiPlayerConfig, FfiPlayerSnapshot, FfiPlayerStatus,
    },
    web::{bridge::WorkerBridge, commands::WorkerCmd},
};

/// Number of EQ bands surfaced through the wasm facade. Mirrors the
/// fixed log-spaced layout the legacy [`Player`](crate::web::player)
/// singleton exposed. Module-level because the struct's `eq_gains` array
/// length references it (a position where `Self::` is not yet in scope);
/// the remaining scalar defaults live as `WasmInner` associated consts.
const EQ_BANDS: usize = 10;

/// Caller-facing ordered queue view: the `(TrackId, item)` pairs the
/// caller inserted, in queue order. The worker owns the canonical
/// [`Queue`](kithara_queue::Queue); this mirror exists because the caller
/// allocates the [`TrackId`](kithara_queue::TrackId) on the main thread
/// and the worker plants the identical id via `*_with_id`, so order is
/// deterministic without a round-trip. Drives `items` / `item_count` /
/// index-based `select_item` exactly as `NativeInner`'s registry +
/// `queue.tracks()` order do on native.
type QueueView = Vec<(kithara_queue::TrackId, Arc<AudioPlayerItem>)>;

/// Wasm implementation of the FFI player engine, parallel to
/// [`NativeInner`](crate::native::inner::NativeInner). Exposes the same
/// inherent method set the [`AudioPlayer`](crate::player) facade delegates
/// to, so the single facade body type-checks on both targets.
///
/// The worker owns the `Arc<Queue>`; `WasmInner` owns the command channel
/// into it plus the main-thread caller-facing state (cached scalar
/// settings + the ordered queue view). Setters write through to both the
/// worker and the local cache so the infallible facade getters can answer
/// synchronously without a worker round-trip.
pub(crate) struct WasmInner {
    bridge: WorkerBridge,
    queue_view: Arc<Mutex<QueueView>>,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    volume: AtomicU32,
    crossfade_secs: AtomicU32,
    playing_rate: AtomicU32,
    muted: Mutex<bool>,
    eq_gains: [AtomicU32; EQ_BANDS],
}

impl Default for WasmInner {
    fn default() -> Self {
        Self {
            bridge: WorkerBridge::default(),
            queue_view: Arc::new(Mutex::new(Vec::new())),
            observer: Mutex::new(None),
            volume: AtomicU32::new(Self::DEFAULT_VOLUME.to_bits()),
            crossfade_secs: AtomicU32::new(Self::DEFAULT_CROSSFADE_SECONDS.to_bits()),
            playing_rate: AtomicU32::new(Self::DEFAULT_PLAYING_RATE.to_bits()),
            muted: Mutex::new(false),
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
    /// Default output volume, matching the legacy wasm player.
    const DEFAULT_VOLUME: f32 = 0.5;
    /// Default crossfade window in seconds, matching the worker default.
    const DEFAULT_CROSSFADE_SECONDS: f32 = 5.0;
    /// Default target playback rate.
    const DEFAULT_PLAYING_RATE: f32 = 1.0;
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;

    pub(crate) fn new(_config: FfiPlayerConfig) -> Self {
        Self::default()
    }

    /// Boot the engine worker eagerly so the first command does not pay
    /// the spawn cost.
    pub(crate) fn start(&self) {
        self.bridge.ensure_worker_started();
    }

    /// Forward a command to the worker, mapping a channel failure to a
    /// typed [`FfiError`]. Used by the fallible facade methods that should
    /// surface a real error when the worker link is down.
    fn try_send(&self, cmd: WorkerCmd) -> Result<(), FfiError> {
        self.bridge.send(cmd).map_err(|err| into_internal(&err))
    }

    /// Forward a command for the infallible facade methods (play / pause /
    /// volume / …), logging a dropped command rather than propagating —
    /// these have no error channel in the facade signature.
    fn send(&self, cmd: WorkerCmd) {
        if let Err(err) = self.bridge.send(cmd) {
            tracing::warn!(?err, "wasm worker command dropped: channel unavailable");
        }
    }

    fn next_request_id() -> u32 {
        crate::web::js::next_request_id()
    }

    fn id_at(&self, index: u32) -> Option<kithara_queue::TrackId> {
        self.queue_view
            .lock_sync()
            .get(index as usize)
            .map(|(id, _)| *id)
    }

    pub(crate) fn advance_to_next_item(&self) {
        let Some(current) = self.bridge.current_track_id() else {
            tracing::warn!(
                "advance_to_next_item is not wired on wasm yet: the worker owns the \
                 current-track cursor (Wave 5)"
            );
            return;
        };
        let next = self
            .queue_view
            .lock_sync()
            .iter()
            .position(|(id, _)| *id == current)
            .and_then(|idx| self.queue_view.lock_sync().get(idx + 1).map(|(id, _)| *id));
        if let Some(next) = next {
            let request_id = Self::next_request_id();
            self.send(WorkerCmd::SelectQueue {
                id: next,
                transition: Transition::None,
                request_id,
            });
        }
    }

    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let id = item.track_id();
        self.try_send(WorkerCmd::Append {
            id,
            url: item.url(),
        })?;
        *item.inserted.lock_sync() = true;
        self.queue_view.lock_sync().push((id, Arc::clone(item)));
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn crossfade_duration(&self) -> f32 {
        load_f32(&self.crossfade_secs)
    }

    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let current = self.bridge.current_track_id()?;
        self.queue_view
            .lock_sync()
            .iter()
            .find(|(id, _)| *id == current)
            .map(|(_, item)| Arc::clone(item))
    }

    pub(crate) fn current_time(&self) -> f64 {
        self.bridge.position_secs()
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
            url: item.url(),
            after: after_id,
            request_id,
        });

        let mut view = self.queue_view.lock_sync();
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

        *item.inserted.lock_sync() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn is_muted(&self) -> bool {
        *self.muted.lock_sync()
    }

    pub(crate) fn item_count(&self) -> u32 {
        let len = self.queue_view.lock_sync().len();
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.queue_view
            .lock_sync()
            .iter()
            .map(|(_, item)| Arc::clone(item))
            .collect()
    }

    pub(crate) fn pause(&self) {
        self.send(WorkerCmd::Pause);
    }

    pub(crate) fn play(&self) {
        self.send(WorkerCmd::Play);
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
        if !*item.inserted.lock_sync() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item {} not in queue", item.audio_id()),
            });
        }
        let id = item.track_id();
        let request_id = Self::next_request_id();
        self.send(WorkerCmd::Remove { id, request_id });
        self.queue_view
            .lock_sync()
            .retain(|(existing, _)| *existing != id);
        *item.inserted.lock_sync() = false;
        Ok(())
    }

    pub(crate) fn remove_all_items(&self) {
        self.send(WorkerCmd::RemoveAll);
        let mut view = self.queue_view.lock_sync();
        for (_, item) in view.drain(..) {
            *item.inserted.lock_sync() = false;
        }
    }

    pub(crate) fn replace_item(
        &self,
        index: u32,
        item: &Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let idx = index as usize;
        let new_id = item.track_id();
        let request_id = Self::next_request_id();

        let mut view = self.queue_view.lock_sync();
        if idx >= view.len() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item index {idx} out of range (len: {})", view.len()),
            });
        }
        self.send(WorkerCmd::Replace {
            index,
            id: new_id,
            url: item.url(),
            request_id,
        });
        if let Some((_, old)) = view.get(idx) {
            *old.inserted.lock_sync() = false;
        }
        view[idx] = (new_id, Arc::clone(item));
        drop(view);

        *item.inserted.lock_sync() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn reset_eq(&self) -> Result<(), FfiError> {
        for g in &self.eq_gains {
            store_f32(g, 0.0);
        }
        self.try_send(WorkerCmd::ResetEq)
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

    pub(crate) fn select_item(
        &self,
        index: u32,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        let id = self.id_at(index).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!(
                "item index {index} out of range (len: {})",
                self.queue_view.lock_sync().len()
            ),
        })?;
        let request_id = Self::next_request_id();
        self.send(WorkerCmd::SelectQueue {
            id,
            transition: Transition::from(transition),
            request_id,
        });
        Ok(())
    }

    pub(crate) fn set_abr_mode(&self, mode: FfiAbrMode) {
        let variant_index = match mode {
            FfiAbrMode::Auto => None,
            FfiAbrMode::Manual { variant_index } => Some(variant_index),
        };
        self.send(WorkerCmd::SetAbrMode { variant_index });
    }

    pub(crate) fn set_crossfade_duration(&self, seconds: f32) {
        store_f32(&self.crossfade_secs, seconds);
        self.send(WorkerCmd::SetCrossfade(seconds));
    }

    pub(crate) fn set_eq_gain(&self, band: u32, gain_db: f32) -> Result<(), FfiError> {
        if let Some(slot) = self.eq_gains.get(band as usize) {
            store_f32(slot, gain_db);
        }
        self.try_send(WorkerCmd::SetEqGain { band, gain_db })
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        *self.muted.lock_sync() = muted;
        let volume = if muted { 0.0 } else { load_f32(&self.volume) };
        self.send(WorkerCmd::SetVolume(volume));
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        crate::web::observer::router::install(Arc::clone(&observer), Arc::clone(&self.queue_view));
        *self.observer.lock_sync() = Some(observer);
    }

    pub(crate) fn set_playing_rate(&self, rate: f32) {
        store_f32(&self.playing_rate, rate);
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        store_f32(&self.volume, volume);
        if !*self.muted.lock_sync() {
            self.send(WorkerCmd::SetVolume(volume));
        }
    }

    pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>) {
        let salt = crate::web::js::generate_salt();
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
        crate::web::key_processor_bridge::install_main_processor(Arc::clone(&rule.processor));
        let salt = rule.salt.unwrap_or_else(crate::web::js::generate_salt);
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
            is_muted: *self.muted.lock_sync(),
        }
    }

    pub(crate) fn stop(&self) {
        self.send(WorkerCmd::Stop);
        let mut view = self.queue_view.lock_sync();
        for (_, item) in view.drain(..) {
            *item.inserted.lock_sync() = false;
        }
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
