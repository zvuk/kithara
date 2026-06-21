#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
use std::sync::Arc;

use kithara_events::TrackId;
use kithara_platform::sync::Mutex;
use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
use crate::native::item_bridge::ItemEventBridge;
#[cfg(not(target_arch = "wasm32"))]
use crate::types::FfiAbrMode;
use crate::{
    observer::{ItemLoadCallback, ItemObserver},
    types::{FfiItemConfig, FfiItemLoadResult, FfiTimeRange},
};

/// Loading lifecycle of an item. A sum type so the contradictory
/// boolean combinations the old packed struct allowed
/// (`ready && failed`, `ready && duration == 0`, `failed && duration`)
/// are unrepresentable: a duration only exists inside `Ready`, and
/// `Ready` / `Failed` are mutually exclusive.
#[derive(Debug, Clone, Copy)]
enum LoadingState {
    /// Inserted but no duration resolved yet (and not failed).
    Pending,
    /// Metadata resolved; `duration_sec` is the playable duration.
    Ready { duration_sec: f64 },
    /// Terminal failure — sticky, carries no duration.
    Failed,
}

/// Cached subset of item state surfaced through synchronous getters
/// (`duration_sec`, `is_live_stream`, …) and the `load()` resolver.
/// Updated by [`ItemEventBridge`] through the typed transition methods
/// as the underlying resource emits metadata events.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ItemView {
    loading: LoadingState,
    has_protected_content: bool,
    is_live_stream: bool,
}

impl ItemView {
    fn new(is_live_stream: bool) -> Self {
        Self {
            is_live_stream,
            loading: LoadingState::Pending,
            has_protected_content: false,
        }
    }

    /// Resolved duration in seconds, or `0.0` when not yet `Ready`
    /// (pending or failed).
    fn duration_sec(&self) -> f64 {
        match self.loading {
            LoadingState::Ready { duration_sec } => duration_sec,
            LoadingState::Pending | LoadingState::Failed => 0.0,
        }
    }

    /// Whether the item resolved metadata and is playable. False while
    /// pending or after a failure — `Ready` and `Failed` are exclusive,
    /// so this is the typed replacement for `is_ready_to_play && !is_failed`.
    fn is_ready(&self) -> bool {
        matches!(self.loading, LoadingState::Ready { .. })
    }

    /// Terminal failure transition from any state.
    pub(crate) fn mark_failed(&mut self) {
        self.loading = LoadingState::Failed;
    }

    /// Metadata resolved with `duration_sec`. A no-op once `Failed`
    /// (failure is sticky), mirroring the old `is_failed` flag never
    /// being cleared.
    pub(crate) fn resolve_duration(&mut self, duration_sec: f64) {
        if !matches!(self.loading, LoadingState::Failed) {
            self.loading = LoadingState::Ready { duration_sec };
        }
    }
}

/// FFI-facing audio player item.
///
/// Carries two identifiers, per iOS `AudioPlayerItemProtocol`:
/// - [`Self::audio_id`] — caller-facing content id. When
///   [`FfiItemConfig::audio_id`] is absent it falls back to the
///   internally allocated queue id for standalone Kithara callers.
/// - [`Self::uuid_i64`] — caller-facing queue-item id. When
///   [`FfiItemConfig::uuid_i64`] is absent it falls back to the
///   legacy UUIDv5-derived handle.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    pub(crate) state: Arc<Mutex<ItemView>>,
    /// Scoped event bus — set by `AudioPlayer::insert` so per-resource
    /// events (Hls/File/Audio) published during `Resource::new` are
    /// captured even when [`set_observer`] is called later. Native-only:
    /// the wasm worker owns the queue and its event bus.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) bus: Mutex<Option<kithara_events::EventBus>>,
    /// Inserted-into-queue flag — flipped by `AudioPlayer::insert` so
    /// [`Self::load`] can tell "still detached" from "loaded enough to
    /// answer playable". Pre-insert / post-remove value is `false`.
    pub(crate) inserted: Mutex<bool>,
    config: FfiItemConfig,
    /// Per-item event bridge translating resource events into
    /// [`ItemObserver`] callbacks. Native-only: the wasm worker routes
    /// item events through the main-thread event router instead (Wave 5).
    #[cfg(not(target_arch = "wasm32"))]
    event_bridge: Mutex<Option<ItemEventBridge>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
    /// Process-wide monotonic id allocated at construction and consumed
    /// by the core queue. Not exposed by the high-level Swift API: it is
    /// only the routing key that lets repeated business tracks coexist.
    queue_id: TrackId,
    /// Caller-facing content id returned by [`Self::audio_id`].
    audio_id: TrackId,
    /// Caller-facing queue-item uuid returned by [`Self::uuid_i64`].
    uuid_i64: i64,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl AudioPlayerItem {
    /// Create a new item with frozen preferences. Reserves a fresh
    /// private queue id from the process-wide counter. Caller-supplied
    /// `audioId` / `uuid` are stored on the item and surfaced through
    /// the iOS-compatible accessors without becoming the core queue key.
    /// Loading starts automatically when the item is inserted into an
    /// [`crate::player::AudioPlayer`].
    #[must_use]
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(config: FfiItemConfig) -> Arc<Self> {
        let live = config.is_live_stream;
        let queue_id = TrackId::allocate();
        let audio_id = config.audio_id.unwrap_or(queue_id);
        let uuid_i64 = config
            .uuid_i64
            .unwrap_or_else(|| derived_uuid_i64(&config.url, queue_id));
        Arc::new(Self {
            config,
            queue_id,
            audio_id,
            uuid_i64,
            #[cfg(not(target_arch = "wasm32"))]
            event_bridge: Mutex::default(),
            observer: Mutex::default(),
            #[cfg(not(target_arch = "wasm32"))]
            bus: Mutex::default(),
            inserted: Mutex::default(),
            state: Arc::new(Mutex::new(ItemView::new(live))),
        })
    }

    /// Caller-facing content id. Mirrors the iOS
    /// `AudioPlayerItemProtocol.audioId: TrackId`.
    pub fn audio_id(&self) -> TrackId {
        self.audio_id
    }

    /// Cached item duration in seconds. Defaults to `0.0` until the
    /// underlying resource emits a duration update.
    pub fn duration_sec(&self) -> f64 {
        self.state.lock().duration_sec()
    }

    /// Whether this item represents a live HLS feed. The flag is set
    /// from [`FfiItemConfig::is_live_stream`] at construction; in the
    /// future this getter will also surface auto-detected live streams.
    pub fn is_live_stream(&self) -> bool {
        self.state.lock().is_live_stream
    }

    /// Whether the item is playable at `progress` (seconds) given the
    /// caller-supplied buffered `ranges`. Live streams are reported
    /// playable unconditionally.
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift requires owned Vec across FFI ABI"
        )
    )]
    pub fn is_playable(&self, progress: f64, ranges: Vec<FfiTimeRange>) -> bool {
        if self.is_live_stream() {
            return true;
        }
        ranges
            .iter()
            .any(|r| progress >= r.start_seconds && progress < r.start_seconds + r.duration_seconds)
    }

    /// Resolve `callback` with the item's current load status. If the
    /// item has not yet been inserted into a queue (or has been
    /// removed), the callback fires with
    /// `FfiItemLoadResult { has_protected_content: false, is_playable: false }`.
    ///
    /// `load` does not trigger an additional fetch — `AudioPlayer::insert`
    /// already kicks off background loading. This method is the FFI
    /// answer to the iOS protocol's `func load() -> Observable<…>`:
    /// it surfaces the cached state once the metadata layer has caught
    /// up.
    #[cfg_attr(
        all(),
        expect(
            clippy::needless_pass_by_value,
            reason = "UniFFI Lift trait requires owned Arc — FFI ABI contract"
        )
    )]
    pub fn load(&self, callback: Arc<dyn ItemLoadCallback>) {
        let inserted = *self.inserted.lock();
        let snapshot = *self.state.lock();
        let result = if inserted {
            FfiItemLoadResult {
                has_protected_content: snapshot.has_protected_content,
                is_playable: snapshot.is_ready(),
            }
        } else {
            FfiItemLoadResult {
                has_protected_content: false,
                is_playable: false,
            }
        };
        callback.on_complete(result);
    }

    pub fn preferred_peak_bitrate(&self) -> f64 {
        self.config.preferred_peak_bitrate
    }

    pub fn preferred_peak_bitrate_for_expensive_networks(&self) -> f64 {
        self.config.preferred_peak_bitrate_expensive
    }

    /// Private queue id used by player-level events to route back to the
    /// Swift-owned item instance. High-level Swift maps it back to
    /// [`Self::audio_id`] before publishing public events.
    pub fn queue_id(&self) -> TrackId {
        self.queue_id
    }

    pub fn set_observer(&self, observer: Arc<dyn ItemObserver>) {
        *self.observer.lock() = Some(observer);
        self.restart_bridge();
    }

    /// Audio source string — either a network URL or an absolute local
    /// path, as supplied via [`FfiItemConfig::url`]. The Swift wrapper
    /// surfaces this as a `URL` (`file://…` for local paths) so the iOS
    /// `AudioPlayerItemProtocol.url` contract holds for both cases.
    pub fn url(&self) -> String {
        self.config.url.clone()
    }

    /// Caller-facing queue-item uuid. Maps to
    /// `AudioPlayerItemProtocol.uuid: Int64` on iOS.
    pub fn uuid_i64(&self) -> i64 {
        self.uuid_i64
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn abr_mode(&self) -> Option<FfiAbrMode> {
        self.config.abr_mode
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn headers(&self) -> Option<HashMap<String, String>> {
        self.config.headers.clone()
    }

    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock().clone()
    }

    /// (Re)subscribe the bridge to the currently-attached scoped bus.
    /// Called from `set_observer` and from `AudioPlayer::insert` right
    /// after the bus is attached.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn restart_bridge(&self) {
        let Some(observer) = self.observer() else {
            *self.event_bridge.lock() = None;
            return;
        };
        let Some(bus) = self.bus.lock().clone() else {
            *self.event_bridge.lock() = None;
            return;
        };
        let bridge = ItemEventBridge::spawn(
            bus.subscribe(),
            observer,
            None,
            Arc::clone(&self.state),
            kithara_platform::CancelToken::never(),
        );
        *self.event_bridge.lock() = Some(bridge);
    }

    /// Wasm has no long-lived per-item bus bridge — the worker owns the
    /// queue and routes events through the main-thread router
    /// ([`crate::web::observer::router`]). What restart still needs to do
    /// is prime a freshly-attached observer with the item's cached
    /// [`ItemState`] so the caller sees the same initial event
    /// (`StatusChanged`) the native path emits when its bridge spawns.
    /// Without this priming, observers attached *after* the worker has
    /// already announced "loaded" would never see the readiness event.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn restart_bridge(&self) {
        let Some(observer) = self.observer() else {
            return;
        };
        let snapshot = *self.state.lock();
        match snapshot.loading {
            LoadingState::Failed => {
                observer.on_event(crate::types::FfiItemEvent::StatusChanged {
                    status: crate::types::FfiItemStatus::Failed,
                });
            }
            LoadingState::Ready { .. } => {
                observer.on_event(crate::types::FfiItemEvent::StatusChanged {
                    status: crate::types::FfiItemStatus::ReadyToPlay,
                });
            }
            LoadingState::Pending => {}
        }
        let duration = snapshot.duration_sec();
        if duration > 0.0 {
            observer.on_event(crate::types::FfiItemEvent::DurationChanged { seconds: duration });
        }
    }

    /// Strongly-typed view of [`Self::audio_id`] for queue calls that
    /// take a [`TrackId`]. The value is identical — just the wrapper
    /// type — so there is no conversion loss across the boundary.
    pub(crate) fn track_id(&self) -> TrackId {
        self.queue_id
    }
}

fn derived_uuid_i64(url: &str, queue_id: TrackId) -> i64 {
    let key = format!("{}:{}", url, queue_id.as_u64());
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&uuid.as_bytes()[0..8]);
    i64::from_be_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_url(url: String) -> FfiItemConfig {
        FfiItemConfig {
            url,
            headers: None,
            audio_id: None,
            uuid_i64: None,
            preferred_peak_bitrate: 0.0,
            preferred_peak_bitrate_expensive: 0.0,
            abr_mode: None,
            is_live_stream: false,
        }
    }

    fn item_for(url: &str) -> Arc<AudioPlayerItem> {
        AudioPlayerItem::new(config_with_url(url.to_string()))
    }

    #[kithara::test]
    fn audio_id_is_monotonic_across_new_items() {
        let a = item_for("https://example.com/a.mp3");
        let b = item_for("https://example.com/b.mp3");
        assert!(a.audio_id() < b.audio_id());
    }

    #[kithara::test]
    fn audio_id_is_distinct_for_two_items_of_same_url() {
        let a = item_for("https://example.com/track.mp3");
        let b = item_for("https://example.com/track.mp3");
        assert_ne!(a.audio_id(), b.audio_id());
    }

    #[kithara::test]
    fn uuid_is_distinct_for_two_items_of_same_url() {
        let a = item_for("https://example.com/track.mp3");
        let b = item_for("https://example.com/track.mp3");
        assert_ne!(a.uuid_i64(), b.uuid_i64());
    }

    #[kithara::test]
    fn uuid_i64_matches_uuid_v5_of_url_and_audio_id() {
        let url = "https://example.com/song.mp3";
        let item = item_for(url);
        let key = format!("{}:{}", url, item.track_id());
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&expected.as_bytes()[0..8]);
        assert_eq!(item.uuid_i64(), i64::from_be_bytes(buf));
    }

    #[kithara::test]
    fn caller_audio_id_is_exposed_without_becoming_queue_id() {
        let config = FfiItemConfig {
            audio_id: Some(TrackId(42)),
            ..config_with_url("https://example.com/song.mp3".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert_eq!(item.audio_id(), TrackId(42));
        assert_ne!(item.track_id(), TrackId(42));
    }

    #[kithara::test]
    fn caller_uuid_i64_is_exposed() {
        let config = FfiItemConfig {
            uuid_i64: Some(123_456),
            ..config_with_url("https://example.com/song.mp3".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert_eq!(item.uuid_i64(), 123_456);
    }

    #[kithara::test]
    fn audio_id_and_uuid_work_for_local_path() {
        let path = "/Users/me/Music/song.flac";
        let a = item_for(path);
        let b = item_for(path);
        assert_ne!(a.audio_id(), b.audio_id());
        assert_ne!(a.uuid_i64(), b.uuid_i64());
    }

    #[kithara::test]
    fn url_preserved() {
        let item = item_for("https://example.com/song.mp3");
        assert_eq!(item.url(), "https://example.com/song.mp3");
    }

    #[kithara::test]
    fn preferred_peak_bitrate_from_config() {
        let config = FfiItemConfig {
            preferred_peak_bitrate: 256_000.0,
            ..config_with_url("https://example.com/a.mp3".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert_eq!(item.preferred_peak_bitrate(), 256_000.0);
    }

    #[kithara::test]
    fn inserted_flag_initially_false() {
        let item = item_for("https://example.com/a.mp3");
        assert!(!*item.inserted.lock());
    }

    #[kithara::test]
    fn headers_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer token".into());
        let config = FfiItemConfig {
            headers: Some(headers),
            ..config_with_url("https://example.com/a.mp3".to_string())
        };
        let item = AudioPlayerItem::new(config);
        let returned = item
            .headers()
            .expect("BUG: headers were just set on the config above");
        assert_eq!(returned.get("Authorization"), Some(&"Bearer token".into()));
    }

    #[kithara::test]
    fn uuid_i64_is_stable_for_same_audio_id() {
        let item = item_for("https://example.com/a.mp3");
        let first = item.uuid_i64();
        let second = item.uuid_i64();
        assert_eq!(first, second);
    }

    #[kithara::test]
    fn is_live_stream_defaults_false() {
        let item = item_for("https://example.com/song.mp3");
        assert!(!item.is_live_stream());
    }

    #[kithara::test]
    fn is_live_stream_from_config() {
        let config = FfiItemConfig {
            is_live_stream: true,
            ..config_with_url("https://example.com/live.m3u8".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert!(item.is_live_stream());
    }

    #[kithara::test]
    fn is_playable_live_stream_always_true() {
        let config = FfiItemConfig {
            is_live_stream: true,
            ..config_with_url("https://example.com/live.m3u8".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert!(item.is_playable(0.0, vec![]));
        assert!(item.is_playable(9999.0, vec![]));
    }

    #[kithara::test]
    fn item_view_pending_is_not_ready_and_zero_duration() {
        let view = ItemView::new(false);
        assert!(!view.is_ready());
        assert_eq!(view.duration_sec(), 0.0);
    }

    #[kithara::test]
    fn item_view_resolve_duration_sets_ready() {
        let mut view = ItemView::new(false);
        view.resolve_duration(42.0);
        assert!(view.is_ready());
        assert_eq!(view.duration_sec(), 42.0);
    }

    #[kithara::test]
    fn item_view_mark_failed_is_not_ready_and_zero_duration() {
        let mut view = ItemView::new(false);
        view.resolve_duration(42.0);
        view.mark_failed();
        assert!(!view.is_ready());
        assert_eq!(view.duration_sec(), 0.0);
    }

    #[kithara::test]
    fn item_view_failure_is_sticky_over_resolve_duration() {
        let mut view = ItemView::new(false);
        view.mark_failed();
        view.resolve_duration(42.0);
        assert!(
            !view.is_ready(),
            "resolve_duration must not un-fail a Failed item"
        );
        assert_eq!(view.duration_sec(), 0.0);
    }

    #[kithara::test]
    fn item_view_live_flag_preserved_across_transitions() {
        let mut view = ItemView::new(true);
        assert!(view.is_live_stream);
        view.resolve_duration(10.0);
        assert!(view.is_live_stream);
        view.mark_failed();
        assert!(view.is_live_stream);
    }

    #[kithara::test]
    fn is_playable_within_ranges() {
        let item = item_for("https://example.com/song.mp3");
        let ranges = vec![FfiTimeRange {
            start_seconds: 0.0,
            duration_seconds: 30.0,
        }];
        assert!(item.is_playable(0.0, ranges.clone()));
        assert!(item.is_playable(15.0, ranges.clone()));
        assert!(!item.is_playable(30.0, ranges.clone()));
        assert!(!item.is_playable(45.0, ranges));
    }
}
