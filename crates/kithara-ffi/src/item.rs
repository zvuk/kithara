//! FFI wrapper for audio player items.
//!
//! `AudioPlayerItem` is a handle, analogous to `AVPlayerItem`. It holds
//! caller-supplied per-item preferences ([`FfiItemConfig`] — URL,
//! headers, bitrate caps, ABR mode). Resource loading is owned by
//! [`crate::player::AudioPlayer`] via `kithara_queue::Queue`:
//!
//! 1. `AudioPlayerItem::new(config)` — creates the handle; preferences
//!    are frozen at construction.
//! 2. `AudioPlayer::insert(item)` — registers the URL with the Queue,
//!    which assigns a [`TrackId`] (stored in the item) and starts the
//!    background load.
//! 3. Per-item events flow through [`set_observer`] — the bridge
//!    subscribes to the scoped event bus attached by `insert`.

use std::{collections::HashMap, sync::Arc};

use kithara_events::TrackId;
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    item_bridge::ItemEventBridge,
    observer::ItemObserver,
    types::{FfiAbrMode, FfiItemConfig},
};

/// FFI-facing audio player item with UUID identity.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    id: Uuid,
    config: FfiItemConfig,
    event_bridge: Mutex<Option<ItemEventBridge>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
    /// Scoped event bus — set by `AudioPlayer::insert` so per-resource
    /// events (Hls/File/Audio) published during `Resource::new` are
    /// captured even when [`set_observer`] is called later.
    pub(crate) bus: Mutex<Option<kithara_events::EventBus>>,
    /// Queue-assigned id — set by `AudioPlayer::insert` once the Queue
    /// accepts the source. `None` before insert, `None` again after
    /// `remove`.
    pub(crate) track_id: Mutex<Option<TrackId>>,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayerItem {
    /// Create a new item with frozen preferences. Loading starts
    /// automatically when the item is inserted into an
    /// [`crate::player::AudioPlayer`].
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(config: FfiItemConfig) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            config,
            event_bridge: Mutex::new(None),
            observer: Mutex::new(None),
            bus: Mutex::new(None),
            track_id: Mutex::new(None),
        })
    }

    /// String representation of the item's unique ID.
    pub fn id(&self) -> String {
        self.id.to_string()
    }

    pub fn url(&self) -> String {
        self.config.url.clone()
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
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock_sync().clone()
    }

    pub(crate) fn headers(&self) -> Option<HashMap<String, String>> {
        self.config.headers.clone()
    }

    pub(crate) fn abr_mode(&self) -> Option<FfiAbrMode> {
        self.config.abr_mode
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
        let bridge =
            ItemEventBridge::spawn(bus.subscribe(), observer, None, CancellationToken::new());
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
    fn new_item_has_unique_id() {
        let a = item_for("https://example.com/a.mp3");
        let b = item_for("https://example.com/b.mp3");
        assert_ne!(a.id(), b.id());
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
        let returned = item.headers().expect("headers set");
        assert_eq!(returned.get("Authorization"), Some(&"Bearer token".into()));
    }
}
