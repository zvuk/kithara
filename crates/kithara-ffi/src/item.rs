use std::{collections::HashMap, sync::Arc};

use kithara_events::TrackId;
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    item_bridge::ItemEventBridge,
    observer::{ItemLoadCallback, ItemObserver},
    types::{FfiAbrMode, FfiItemConfig, FfiItemLoadResult, FfiTimeRange},
};

/// Cached subset of item state surfaced through synchronous getters
/// (`duration_sec`, `is_live_stream`, …) and the `load()` resolver.
/// Updated by [`ItemEventBridge`] as the underlying resource emits
/// metadata events.
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct ItemState {
    pub has_protected_content: bool,
    pub is_failed: bool,
    pub is_live_stream: bool,
    pub is_ready_to_play: bool,
    pub duration_sec: f64,
}

/// FFI-facing audio player item.
///
/// Carries two identifiers, per iOS `AudioPlayerItemProtocol`:
/// - [`Self::audio_id`] — monotonic [`TrackId`] (`u64`) reserved at
///   construction via [`TrackId::allocate`]. The queue consumes the
///   same value via
///   [`kithara_queue::Queue::insert_with_id`] / `append_with_id`, so
///   there is exactly one address space across the FFI ↔ core
///   boundary. This is `audioId: TrackId` on iOS.
/// - [`Self::uuid_i64`] — `i64` derived from a per-item `UUIDv5` over
///   `url + audio_id`. Stable for the item's lifetime and distinct
///   for every fresh insertion of the same URL. This is
///   `uuid: Int64` on iOS.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    pub(crate) state: Arc<Mutex<ItemState>>,
    /// Scoped event bus — set by `AudioPlayer::insert` so per-resource
    /// events (Hls/File/Audio) published during `Resource::new` are
    /// captured even when [`set_observer`] is called later.
    pub(crate) bus: Mutex<Option<kithara_events::EventBus>>,
    /// Inserted-into-queue flag — flipped by `AudioPlayer::insert` so
    /// [`Self::load`] can tell "still detached" from "loaded enough to
    /// answer playable". Pre-insert / post-remove value is `false`.
    pub(crate) inserted: Mutex<bool>,
    config: FfiItemConfig,
    event_bridge: Mutex<Option<ItemEventBridge>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
    /// Process-wide monotonic id allocated at construction. Surfaces
    /// through [`Self::audio_id`] and consumed by the queue.
    id: TrackId,
    /// `UUIDv5` over `format!("{url}:{audio_id}")`. Two items with the
    /// same URL but different monotonic [`Self::id`] get different
    /// uuids, so the queue can distinguish independent insertions.
    /// Computed once in [`Self::new`] for `O(1)` accessor cost.
    uuid: Uuid,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayerItem {
    /// Create a new item with frozen preferences. Reserves a fresh
    /// [`TrackId`] from the process-wide counter so `audioId` is stable
    /// from this point on, and derives a `UUIDv5` over
    /// `format!("{url}:{audio_id}")` for the secondary `uuid` handle.
    /// Loading starts automatically when the item is inserted into an
    /// [`crate::player::AudioPlayer`].
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(config: FfiItemConfig) -> Arc<Self> {
        let live = config.is_live_stream;
        let id = TrackId::allocate();
        let key = format!("{}:{}", config.url, id.as_u64());
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
        Arc::new(Self {
            config,
            id,
            uuid,
            event_bridge: Mutex::new(None),
            observer: Mutex::new(None),
            bus: Mutex::new(None),
            inserted: Mutex::new(false),
            state: Arc::new(Mutex::new(ItemState {
                is_live_stream: live,
                ..ItemState::default()
            })),
        })
    }

    /// Monotonic per-item identifier reserved at construction. Mirrors
    /// the iOS `AudioPlayerItemProtocol.audioId: TrackId`. Same value
    /// the queue uses internally — see [`Self::new`] for the
    /// allocation contract.
    pub fn audio_id(&self) -> TrackId {
        self.id
    }

    /// Cached item duration in seconds. Defaults to `0.0` until the
    /// underlying resource emits a duration update.
    pub fn duration_sec(&self) -> f64 {
        self.state.lock_sync().duration_sec
    }

    /// Whether this item represents a live HLS feed. The flag is set
    /// from [`FfiItemConfig::is_live_stream`] at construction; in the
    /// future this getter will also surface auto-detected live streams.
    pub fn is_live_stream(&self) -> bool {
        self.state.lock_sync().is_live_stream
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
        let inserted = *self.inserted.lock_sync();
        let snapshot = *self.state.lock_sync();
        let result = if inserted {
            FfiItemLoadResult {
                has_protected_content: snapshot.has_protected_content,
                is_playable: snapshot.is_ready_to_play && !snapshot.is_failed,
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

    pub fn set_observer(&self, observer: Arc<dyn ItemObserver>) {
        *self.observer.lock_sync() = Some(observer);
        self.restart_bridge();
    }

    /// Audio source string — either a network URL or an absolute local
    /// path, as supplied via [`FfiItemConfig::url`]. The Swift wrapper
    /// surfaces this as a `URL` (`file://…` for local paths) so the iOS
    /// `AudioPlayerItemProtocol.url` contract holds for both cases.
    pub fn url(&self) -> String {
        self.config.url.clone()
    }

    /// Signed-integer view of the per-item `UUIDv5` (`url + audioId`).
    /// Maps to `AudioPlayerItemProtocol.uuid: Int64` on iOS. Distinct
    /// from [`Self::audio_id`]: two items with the same URL but
    /// different monotonic ids produce different `uuid_i64`s, so the
    /// queue can distinguish independent insertions even when the
    /// caller has not reset state in between.
    pub fn uuid_i64(&self) -> i64 {
        let bytes = self.uuid.as_bytes();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[0..8]);
        i64::from_be_bytes(buf)
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    pub(crate) fn abr_mode(&self) -> Option<FfiAbrMode> {
        self.config.abr_mode
    }

    /// Strongly-typed view of [`Self::audio_id`] for queue calls that
    /// take a [`TrackId`]. The value is identical — just the wrapper
    /// type — so there is no conversion loss across the boundary.
    pub(crate) fn track_id(&self) -> TrackId {
        self.id
    }

    pub(crate) fn headers(&self) -> Option<HashMap<String, String>> {
        self.config.headers.clone()
    }

    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock_sync().clone()
    }

    /// (Re)subscribe the bridge to the currently-attached scoped bus.
    /// Called from `set_observer` and from `AudioPlayer::insert` right
    /// after the bus is attached.
    pub(crate) fn restart_bridge(&self) {
        let Some(observer) = self.observer() else {
            *self.event_bridge.lock_sync() = None;
            return;
        };
        let Some(bus) = self.bus.lock_sync().clone() else {
            *self.event_bridge.lock_sync() = None;
            return;
        };
        let bridge = ItemEventBridge::spawn(
            bus.subscribe(),
            observer,
            None,
            Arc::clone(&self.state),
            CancellationToken::new(), // kithara:cancel:bridge
        );
        *self.event_bridge.lock_sync() = Some(bridge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_for(url: &str) -> Arc<AudioPlayerItem> {
        AudioPlayerItem::new(FfiItemConfig::with_url(url.to_string()))
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
        let key = format!("{}:{}", url, item.audio_id());
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&expected.as_bytes()[0..8]);
        assert_eq!(item.uuid_i64(), i64::from_be_bytes(buf));
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
            ..FfiItemConfig::with_url("https://example.com/a.mp3".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert_eq!(item.preferred_peak_bitrate(), 256_000.0);
    }

    #[kithara::test]
    fn inserted_flag_initially_false() {
        let item = item_for("https://example.com/a.mp3");
        assert!(!*item.inserted.lock_sync());
    }

    #[kithara::test]
    fn headers_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer token".into());
        let config = FfiItemConfig {
            headers: Some(headers),
            ..FfiItemConfig::with_url("https://example.com/a.mp3".to_string())
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
            ..FfiItemConfig::with_url("https://example.com/live.m3u8".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert!(item.is_live_stream());
    }

    #[kithara::test]
    fn is_playable_live_stream_always_true() {
        let config = FfiItemConfig {
            is_live_stream: true,
            ..FfiItemConfig::with_url("https://example.com/live.m3u8".to_string())
        };
        let item = AudioPlayerItem::new(config);
        assert!(item.is_playable(0.0, vec![]));
        assert!(item.is_playable(9999.0, vec![]));
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
