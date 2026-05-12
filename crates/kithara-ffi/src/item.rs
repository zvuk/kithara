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

/// FFI-facing audio player item with UUID identity.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    pub(crate) state: Arc<Mutex<ItemState>>,
    /// Scoped event bus — set by `AudioPlayer::insert` so per-resource
    /// events (Hls/File/Audio) published during `Resource::new` are
    /// captured even when [`set_observer`] is called later.
    pub(crate) bus: Mutex<Option<kithara_events::EventBus>>,
    /// Queue-assigned id — set by `AudioPlayer::insert` once the Queue
    /// accepts the source. `None` before insert, `None` again after
    /// `remove`.
    pub(crate) track_id: Mutex<Option<TrackId>>,
    config: FfiItemConfig,
    event_bridge: Mutex<Option<ItemEventBridge>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
    id: Uuid,
}

/// Number of leading hex digits taken from the UUID to derive
/// [`AudioPlayerItem::uuid_i64`]. Must be 16 (i.e. 64 bits).
const UUID_HEX_PREFIX_LEN: usize = 16;

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayerItem {
    /// Create a new item with frozen preferences. Loading starts
    /// automatically when the item is inserted into an
    /// [`crate::player::AudioPlayer`].
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(config: FfiItemConfig) -> Arc<Self> {
        let live = config.is_live_stream;
        Arc::new(Self {
            config,
            id: Uuid::new_v4(),
            event_bridge: Mutex::new(None),
            observer: Mutex::new(None),
            bus: Mutex::new(None),
            track_id: Mutex::new(None),
            state: Arc::new(Mutex::new(ItemState {
                is_live_stream: live,
                ..ItemState::default()
            })),
        })
    }

    /// Stable per-item identifier (`UUIDv4` string). Mirrors the iOS
    /// `AudioPlayerItemProtocol.audioId`.
    pub fn audio_id(&self) -> String {
        self.id.to_string()
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
        let inserted = self.track_id.lock_sync().is_some();
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

    pub fn url(&self) -> String {
        self.config.url.clone()
    }

    /// 64-bit numeric form of [`Self::audio_id`]. Derived from the
    /// first 16 hex digits of the UUID — stable for the same UUID, but
    /// **not** cryptographically unique. Treat as an opaque numeric
    /// handle.
    pub fn uuid_i64(&self) -> i64 {
        let hex: String = self
            .audio_id()
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(UUID_HEX_PREFIX_LEN)
            .collect();
        let raw = u64::from_str_radix(&hex, 16).unwrap_or(0);
        raw as i64
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    pub(crate) fn abr_mode(&self) -> Option<FfiAbrMode> {
        self.config.abr_mode
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
    fn new_item_has_unique_audio_id() {
        let a = item_for("https://example.com/a.mp3");
        let b = item_for("https://example.com/b.mp3");
        assert_ne!(a.audio_id(), b.audio_id());
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
    fn track_id_initially_none() {
        let item = item_for("https://example.com/a.mp3");
        assert!(item.track_id.lock_sync().is_none());
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
    fn uuid_i64_differs_across_items() {
        let a = item_for("https://example.com/a.mp3");
        let b = item_for("https://example.com/b.mp3");
        assert_ne!(a.uuid_i64(), b.uuid_i64());
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
