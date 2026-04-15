//! FFI wrapper for audio player items.
//!
//! `AudioPlayerItem` is a handle, analogous to `AVPlayerItem`. It holds
//! the source URL plus caller-supplied preferences (headers, preferred
//! peak bitrate, ABR mode, store options). The actual resource loading
//! is owned by [`crate::player::AudioPlayer`] via `kithara_queue::Queue`:
//!
//! 1. `AudioPlayerItem::new(url, headers)` — creates the handle.
//! 2. `AudioPlayer::insert(item)` — registers the URL with the Queue, which
//!    assigns a [`TrackId`] (stored in the item) and starts background load.
//! 3. Per-item events flow through [`set_observer`] — the bridge subscribes
//!    to the scoped event bus attached by `AudioPlayer::insert`.

use std::{collections::HashMap, sync::Arc};

use kithara_events::TrackId;
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    config::StoreOptions, item_bridge::ItemEventBridge, observer::ItemObserver, types::FfiAbrMode,
};

/// FFI-facing audio player item with UUID identity.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    id: Uuid,
    url: String,
    headers: Option<HashMap<String, String>>,
    preferred_peak_bitrate: Mutex<f64>,
    preferred_peak_bitrate_expensive: Mutex<f64>,
    abr_mode: Mutex<Option<FfiAbrMode>>,
    store: Mutex<StoreOptions>,
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
    /// Create a new item. Loading starts automatically when the item is
    /// inserted into an [`crate::player::AudioPlayer`].
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(url: String, additional_headers: Option<HashMap<String, String>>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            url,
            headers: additional_headers,
            preferred_peak_bitrate: Mutex::new(0.0),
            preferred_peak_bitrate_expensive: Mutex::new(0.0),
            abr_mode: Mutex::new(None),
            store: Mutex::new(StoreOptions::default()),
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
        self.url.clone()
    }

    pub fn preferred_peak_bitrate(&self) -> f64 {
        *self.preferred_peak_bitrate.lock_sync()
    }

    pub fn set_preferred_peak_bitrate(&self, bitrate: f64) {
        *self.preferred_peak_bitrate.lock_sync() = bitrate;
    }

    pub fn preferred_peak_bitrate_for_expensive_networks(&self) -> f64 {
        *self.preferred_peak_bitrate_expensive.lock_sync()
    }

    pub fn set_preferred_peak_bitrate_for_expensive_networks(&self, bitrate: f64) {
        *self.preferred_peak_bitrate_expensive.lock_sync() = bitrate;
    }

    /// Set ABR mode. Must be called before inserting into the player.
    pub fn set_abr_mode(&self, mode: FfiAbrMode) {
        *self.abr_mode.lock_sync() = Some(mode);
    }

    /// No-op kept for API compatibility. Queue-backed player loads
    /// automatically on [`crate::player::AudioPlayer::insert`].
    pub fn load(self: Arc<Self>) {
        // Queue owns loading; item holds no Resource of its own.
    }

    pub fn set_observer(&self, observer: Arc<dyn ItemObserver>) {
        *self.observer.lock_sync() = Some(observer);
        self.restart_bridge();
    }

    pub fn set_store_options(&self, store: StoreOptions) {
        *self.store.lock_sync() = store;
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock_sync().clone()
    }

    pub(crate) fn headers(&self) -> Option<HashMap<String, String>> {
        self.headers.clone()
    }

    pub(crate) fn store_options(&self) -> StoreOptions {
        self.store.lock_sync().clone()
    }

    pub(crate) fn abr_mode(&self) -> Option<FfiAbrMode> {
        *self.abr_mode.lock_sync()
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

    #[kithara::test]
    fn new_item_has_unique_id() {
        let a = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let b = AudioPlayerItem::new("https://example.com/b.mp3".into(), None);
        assert_ne!(a.id(), b.id());
    }

    #[kithara::test]
    fn url_preserved() {
        let item = AudioPlayerItem::new("https://example.com/song.mp3".into(), None);
        assert_eq!(item.url(), "https://example.com/song.mp3");
    }

    #[kithara::test]
    fn preferred_peak_bitrate_roundtrip() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        assert_eq!(item.preferred_peak_bitrate(), 0.0);
        item.set_preferred_peak_bitrate(256_000.0);
        assert_eq!(item.preferred_peak_bitrate(), 256_000.0);
    }

    #[kithara::test]
    fn load_before_insert_does_not_panic() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        item.load();
    }

    #[kithara::test]
    fn track_id_initially_none() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        assert!(item.track_id.lock_sync().is_none());
    }

    #[kithara::test]
    fn headers_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer token".into());
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), Some(headers));
        let returned = item.headers().expect("headers set");
        assert_eq!(returned.get("Authorization"), Some(&"Bearer token".into()));
    }
}
