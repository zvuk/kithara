#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
use kithara::{events::EventBus, platform::CancelToken};
use kithara::{
    events::TrackId,
    platform::sync::{Arc, Mutex},
};
use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
use crate::native::{ItemEventBridge, ItemTracker};
#[cfg(not(target_arch = "wasm32"))]
use crate::types::FfiAbrMode;
use crate::{
    core::observer_set::ObserverSet,
    observer::{ItemLoadCallback, ItemObserver},
    types::{
        FfiItemConfig, FfiItemEvent, FfiItemLoadResult, FfiItemState, FfiItemStatus, FfiTimeRange,
        FfiTrackStatus,
    },
};

/// Loading lifecycle of an item. A sum type so the contradictory
/// boolean combinations the old packed struct allowed
/// (`ready && failed`, `failed && duration`) are unrepresentable: a
/// duration only exists inside `Ready`, and `Ready` / `Failed` are
/// mutually exclusive.
#[derive(Debug, Clone, Copy)]
enum LoadingState {
    /// Inserted but not playable yet (and not failed).
    Pending,
    /// Playable; `duration_sec` is known once the metadata layer answers.
    Ready { duration_sec: Option<f64> },
    /// Terminal failure — sticky, carries no duration.
    Failed,
}

/// Cached subset of item state surfaced through synchronous getters
/// (`duration_sec`, `is_live_stream`, …) and the `load()` resolver.
/// Updated by [`ItemEventBridge`] through the typed transition methods
/// as the underlying resource emits metadata events.
#[derive(Debug)]
pub(crate) struct ItemView {
    loading: LoadingState,
    error: Option<String>,
    loaded_ranges: Vec<FfiTimeRange>,
    has_protected_content: bool,
    is_live_stream: bool,
}

impl ItemView {
    const fn new(is_live_stream: bool) -> Self {
        Self {
            is_live_stream,
            loading: LoadingState::Pending,
            has_protected_content: false,
            error: None,
            loaded_ranges: Vec::new(),
        }
    }

    /// Folds the state an event carries into the view before observers
    /// see the event.
    pub(crate) fn absorb(&mut self, event: &FfiItemEvent) {
        match event {
            FfiItemEvent::DurationChanged { seconds } => self.resolve_duration(*seconds),
            FfiItemEvent::LoadedRangesChanged { ranges } => self.loaded_ranges.clone_from(ranges),
            _ => {}
        }
    }

    /// Resolved duration in seconds; `None` while pending, failed, or
    /// playable without metadata.
    const fn duration(&self) -> Option<f64> {
        match self.loading {
            LoadingState::Ready { duration_sec } => duration_sec,
            LoadingState::Pending | LoadingState::Failed => None,
        }
    }

    /// [`Self::duration`] for the synchronous getter, `0.0` when unknown.
    fn duration_sec(&self) -> f64 {
        self.duration().unwrap_or(0.0)
    }

    /// Whether the item resolved metadata and is playable. False while
    /// pending or after a failure — `Ready` and `Failed` are exclusive,
    /// so this is the typed replacement for `is_ready_to_play && !is_failed`.
    const fn is_ready(&self) -> bool {
        matches!(self.loading, LoadingState::Ready { .. })
    }

    /// Terminal failure transition from any state. Reports whether this call
    /// performed it, so the second source of a terminal event sees `false` and
    /// stays silent instead of repeating the status/error pair.
    #[must_use]
    pub(crate) fn mark_failed(&mut self, reason: &str) -> bool {
        if matches!(self.loading, LoadingState::Failed) {
            return false;
        }
        self.loading = LoadingState::Failed;
        self.error = Some(reason.to_owned());
        true
    }

    /// Playable without a resolved duration yet: the queue reports a track
    /// loaded before the metadata layer answers. Reports whether this call
    /// changed the state, so a repeated or post-failure `Loaded` stays
    /// silent.
    #[must_use]
    pub(crate) const fn mark_ready(&mut self) -> bool {
        if matches!(self.loading, LoadingState::Pending) {
            self.loading = LoadingState::Ready { duration_sec: None };
            return true;
        }
        false
    }

    /// Metadata resolved with `duration_sec`. A no-op once `Failed`
    /// (failure is sticky), mirroring the old `is_failed` flag never
    /// being cleared.
    const fn resolve_duration(&mut self, duration_sec: f64) {
        if !matches!(self.loading, LoadingState::Failed) {
            self.loading = LoadingState::Ready {
                duration_sec: Some(duration_sec),
            };
        }
    }

    pub(crate) const fn status(&self) -> FfiItemStatus {
        match self.loading {
            LoadingState::Pending => FfiItemStatus::Unknown,
            LoadingState::Ready { .. } => FfiItemStatus::ReadyToPlay,
            LoadingState::Failed => FfiItemStatus::Failed,
        }
    }
}

/// Terminal failure delivered once: whichever source settles the item
/// first emits the status/error pair, and any later source finds it
/// already failed and stays silent. A protocol failure reaches the item
/// through the per-item bridge carrying the exact reason and usually
/// arrives first; a queue failure with no protocol event behind it (a
/// decode, DRM, or storage refusal) still lands here.
pub(crate) fn settle_failed(state: &Mutex<ItemView>, observer: &dyn ItemObserver, reason: &str) {
    if !state.lock().mark_failed(reason) {
        return;
    }
    observer.on_event(FfiItemEvent::StatusChanged {
        status: FfiItemStatus::Failed,
    });
    observer.on_event(FfiItemEvent::Error {
        error: reason.to_owned(),
    });
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
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AudioPlayerItem {
    pub(crate) state: Arc<Mutex<ItemView>>,
    /// Scoped event bus — set by `AudioPlayer::insert` so per-resource
    /// events (Hls/File/Audio) published during `Resource::new` are
    /// captured even when [`Self::add_observer`] is called later. Native-only:
    /// the wasm worker owns the queue and its event bus.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) bus: Mutex<Option<EventBus>>,
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
    observers: Arc<ObserverSet>,
    /// Process-wide monotonic id allocated at construction and consumed
    /// by the core queue. Not exposed by the high-level Swift API: it is
    /// only the routing key that lets repeated business tracks coexist.
    #[field(get(
        name = track_id,
        copy,
        vis = "pub(crate)",
        doc = "Returns the strongly typed queue routing id."
    ))]
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
            observers: Arc::default(),
            #[cfg(not(target_arch = "wasm32"))]
            bus: Mutex::default(),
            inserted: Mutex::default(),
            state: Arc::new(Mutex::new(ItemView::new(live))),
        })
    }

    /// Subscribes `observer` to this item's events and returns the handle
    /// that [`Self::remove_observer`] unsubscribes it with. Every registered
    /// observer receives every event.
    pub fn add_observer(&self, observer: Arc<dyn ItemObserver>) -> u64 {
        #[cfg(target_arch = "wasm32")]
        self.prime(&observer);
        self.observers.add(observer)
    }

    /// Caller-facing content id. Mirrors the iOS
    /// `AudioPlayerItemProtocol.audioId: TrackId`.
    pub const fn audio_id(&self) -> TrackId {
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
        let result = if inserted {
            let view = self.state.lock();
            FfiItemLoadResult {
                has_protected_content: view.has_protected_content,
                is_playable: view.is_ready(),
            }
        } else {
            FfiItemLoadResult {
                has_protected_content: false,
                is_playable: false,
            }
        };
        callback.on_complete(result);
    }

    pub const fn preferred_peak_bitrate(&self) -> f64 {
        self.config.preferred_peak_bitrate
    }

    pub const fn preferred_peak_bitrate_for_expensive_networks(&self) -> f64 {
        self.config.preferred_peak_bitrate_expensive
    }

    /// Private queue id used by player-level events to route back to the
    /// Swift-owned item instance. High-level Swift maps it back to
    /// [`Self::audio_id`] before publishing public events.
    pub const fn queue_id(&self) -> TrackId {
        self.queue_id
    }

    /// Unsubscribes the observer registered under `id`.
    pub fn remove_observer(&self, id: u64) {
        self.observers.remove(id);
    }

    /// Consistent snapshot of status, duration, failure reason and
    /// buffered ranges.
    pub fn state(&self) -> FfiItemState {
        let view = self.state.lock();
        FfiItemState {
            status: view.status(),
            duration_seconds: view.duration(),
            error: view.error.clone(),
            loaded_ranges: view.loaded_ranges.clone(),
        }
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
    pub const fn uuid_i64(&self) -> i64 {
        self.uuid_i64
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) const fn abr_mode(&self) -> Option<FfiAbrMode> {
        self.config.abr_mode
    }

    /// The one place a queue-level track status reaches item state and
    /// item observers, on every platform.
    pub(crate) fn apply_track_status(&self, status: &FfiTrackStatus) {
        match status {
            FfiTrackStatus::Loaded => {
                if self.state.lock().mark_ready() {
                    self.observers.on_event(FfiItemEvent::StatusChanged {
                        status: FfiItemStatus::ReadyToPlay,
                    });
                }
            }
            FfiTrackStatus::Failed { reason } => self.settle_failed(reason),
            _ => {}
        }
    }

    /// The one entry for item events on every platform: folds `event`
    /// into item state, then delivers it to every registered observer.
    pub(crate) fn deliver(&self, event: FfiItemEvent) {
        self.state.lock().absorb(&event);
        self.observers.on_event(event);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn headers(&self) -> Option<HashMap<String, String>> {
        self.config.headers.clone()
    }

    /// The observer set as one trait object, for bridges that own their
    /// sink.
    fn observer(&self) -> Arc<dyn ItemObserver> {
        Arc::clone(&self.observers) as Arc<dyn ItemObserver>
    }

    #[cfg(target_arch = "wasm32")]
    fn prime(&self, observer: &Arc<dyn ItemObserver>) {
        let snapshot = self.state();
        if snapshot.status != FfiItemStatus::Unknown {
            observer.on_event(FfiItemEvent::StatusChanged {
                status: snapshot.status,
            });
        }
        if let Some(seconds) = snapshot.duration_seconds {
            observer.on_event(FfiItemEvent::DurationChanged { seconds });
        }
    }

    /// (Re)subscribe the bridge to the currently-attached scoped bus.
    /// Called from `AudioPlayer::insert` right after the bus is attached.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn restart_bridge(&self) {
        let Some(bus) = self.bus.lock().clone() else {
            *self.event_bridge.lock() = None;
            return;
        };
        let bridge = ItemEventBridge::spawn(
            bus.subscribe(),
            ItemTracker::new(self.observer(), Arc::clone(&self.state)),
            CancelToken::never(),
        );
        *self.event_bridge.lock() = Some(bridge);
    }

    /// Wasm has no long-lived per-item bus bridge — the worker owns the
    /// queue and routes events through the main-thread router
    /// ([`crate::web::observer::router`]). What restart still needs to do
    /// is replay the item's cached [`ItemView`] to its observers, so they
    /// see the same initial event (`StatusChanged`) the native path emits
    /// when its bridge spawns.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn restart_bridge(&self) {
        self.prime(&self.observer());
    }

    /// [`settle_failed`] for this item's own state and observers.
    pub(crate) fn settle_failed(&self, reason: &str) {
        settle_failed(&self.state, self.observers.as_ref(), reason);
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

    fn item_for(url: &str) -> Arc<AudioPlayerItem> {
        AudioPlayerItem::new(FfiItemConfig::for_test(url))
    }

    #[derive(Default)]
    struct CountingObserver {
        seen: Mutex<Vec<String>>,
    }

    impl CountingObserver {
        fn seen(&self) -> Vec<String> {
            self.seen.lock().clone()
        }
    }

    impl ItemObserver for CountingObserver {
        fn on_event(&self, event: FfiItemEvent) {
            self.seen.lock().push(format!("{event:?}"));
        }
    }

    #[kithara::test]
    fn every_registered_observer_sees_the_event_until_it_unsubscribes() {
        let item = item_for("https://example.com/a.mp3");
        let first = Arc::new(CountingObserver::default());
        let second = Arc::new(CountingObserver::default());
        let first_id = item.add_observer(Arc::clone(&first) as Arc<dyn ItemObserver>);
        item.add_observer(Arc::clone(&second) as Arc<dyn ItemObserver>);

        item.deliver(FfiItemEvent::DurationChanged { seconds: 1.0 });
        assert_eq!(first.seen().len(), 1);
        assert_eq!(second.seen().len(), 1);

        item.remove_observer(first_id);
        item.deliver(FfiItemEvent::DurationChanged { seconds: 2.0 });
        assert_eq!(first.seen().len(), 1);
        assert_eq!(second.seen().len(), 2);
    }

    #[kithara::test]
    fn loaded_track_status_marks_the_item_ready_and_tells_observers() {
        let item = item_for("https://example.com/a.mp3");
        let observer = Arc::new(CountingObserver::default());
        item.add_observer(Arc::clone(&observer) as Arc<dyn ItemObserver>);

        item.apply_track_status(&FfiTrackStatus::Loaded);

        assert_eq!(item.state().status, FfiItemStatus::ReadyToPlay);
        assert_eq!(
            observer.seen(),
            vec![format!(
                "{:?}",
                FfiItemEvent::StatusChanged {
                    status: FfiItemStatus::ReadyToPlay
                }
            )]
        );
    }

    #[kithara::test]
    fn loaded_after_failure_leaves_the_item_failed_and_silent() {
        let item = item_for("https://example.com/a.mp3");
        let observer = Arc::new(CountingObserver::default());
        item.add_observer(Arc::clone(&observer) as Arc<dyn ItemObserver>);
        item.apply_track_status(&FfiTrackStatus::Failed {
            reason: "decoder refused".to_owned(),
        });
        let after_failure = observer.seen().len();

        item.apply_track_status(&FfiTrackStatus::Loaded);
        item.apply_track_status(&FfiTrackStatus::Loaded);

        assert_eq!(item.state().status, FfiItemStatus::Failed);
        assert_eq!(observer.seen().len(), after_failure);
    }

    #[kithara::test]
    fn repeated_failed_track_status_emits_the_pair_once() {
        let item = item_for("https://example.com/a.mp3");
        let observer = Arc::new(CountingObserver::default());
        item.add_observer(Arc::clone(&observer) as Arc<dyn ItemObserver>);
        let failed = FfiTrackStatus::Failed {
            reason: "storage refused".to_owned(),
        };

        item.apply_track_status(&failed);
        item.apply_track_status(&failed);

        let state = item.state();
        assert_eq!(state.status, FfiItemStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("storage refused"));
        assert_eq!(observer.seen().len(), 2);
    }

    #[kithara::test]
    fn delivered_events_settle_into_state_before_observers_see_them() {
        let item = item_for("https://example.com/a.mp3");
        let ranges = vec![FfiTimeRange {
            start_seconds: 0.0,
            duration_seconds: 3.0,
        }];

        item.deliver(FfiItemEvent::DurationChanged { seconds: 42.0 });
        item.deliver(FfiItemEvent::LoadedRangesChanged {
            ranges: ranges.clone(),
        });

        let state = item.state();
        assert_eq!(state.duration_seconds, Some(42.0));
        assert_eq!(state.loaded_ranges, ranges);
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
            ..FfiItemConfig::for_test("https://example.com/song.mp3")
        };
        let item = AudioPlayerItem::new(config);
        assert_eq!(item.audio_id(), TrackId(42));
        assert_ne!(item.track_id(), TrackId(42));
    }

    #[kithara::test]
    fn caller_uuid_i64_is_exposed() {
        let config = FfiItemConfig {
            uuid_i64: Some(123_456),
            ..FfiItemConfig::for_test("https://example.com/song.mp3")
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
            ..FfiItemConfig::for_test("https://example.com/a.mp3")
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
            ..FfiItemConfig::for_test("https://example.com/a.mp3")
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
            ..FfiItemConfig::for_test("https://example.com/live.m3u8")
        };
        let item = AudioPlayerItem::new(config);
        assert!(item.is_live_stream());
    }

    #[kithara::test]
    fn is_playable_live_stream_always_true() {
        let config = FfiItemConfig {
            is_live_stream: true,
            ..FfiItemConfig::for_test("https://example.com/live.m3u8")
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
        assert!(view.mark_failed("test failure"));
        assert!(!view.is_ready());
        assert_eq!(view.duration_sec(), 0.0);
    }

    /// Two independent sources settle a failed item — the protocol bridge and
    /// the queue — and each asks the view whether the pair is still its to
    /// emit. Only the transition itself may answer yes.
    #[kithara::test]
    fn item_view_mark_failed_reports_only_the_first_transition() {
        let mut view = ItemView::new(false);
        assert!(view.mark_failed("test failure"));
        assert!(!view.mark_failed("test failure"));
    }

    #[kithara::test]
    fn item_view_failure_is_sticky_over_resolve_duration() {
        let mut view = ItemView::new(false);
        assert!(view.mark_failed("test failure"));
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
        assert!(view.mark_failed("test failure"));
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
