//! FFI wrapper for audio player items.

use std::{collections::HashMap, sync::Arc};

use kithara::play::{Resource, ResourceConfig};
use kithara_platform::Mutex;
use uuid::Uuid;

use crate::{
    observer::ItemObserver,
    types::{FfiError, FfiResult},
};

/// FFI-facing audio player item with UUID identity and lazy loading.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayerItem {
    id: Uuid,
    url: String,
    headers: Option<HashMap<String, String>>,
    preferred_peak_bitrate: Mutex<f64>,
    preferred_peak_bitrate_expensive: Mutex<f64>,
    resource: Mutex<Option<Resource>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayerItem {
    /// Create a new item. Does not start loading — call [`load`] explicitly.
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new(url: String, additional_headers: Option<HashMap<String, String>>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            url,
            headers: additional_headers,
            preferred_peak_bitrate: Mutex::new(0.0),
            preferred_peak_bitrate_expensive: Mutex::new(0.0),
            resource: Mutex::new(None),
            observer: Mutex::new(None),
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

    /// Asynchronously create the underlying [`Resource`].
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::Internal`] if the URL is invalid or resource creation fails.
    pub async fn load(&self) -> Result<(), FfiError> {
        let mut config = ResourceConfig::new(&self.url).map_err(|e| FfiError::Internal {
            message: e.to_string(),
        })?;

        let bitrate = self.preferred_peak_bitrate();
        if bitrate > 0.0 {
            config = config.with_preferred_peak_bitrate(bitrate);
        }

        if let Some(headers) = &self.headers {
            config = config.with_headers(headers.clone().into());
        }

        // Spawn on FFI_RUNTIME so the future runs within a Tokio reactor context.
        // UniFFI polls async futures on its own thread pool which lacks a Tokio runtime.
        let resource = crate::FFI_RUNTIME
            .spawn(Resource::new(config))
            .await
            .map_err(|e| FfiError::Internal {
                message: e.to_string(),
            })?
            .map_err(|e| FfiError::Internal {
                message: e.to_string(),
            })?;

        *self.resource.lock_sync() = Some(resource);
        Ok(())
    }

    pub fn set_observer(&self, observer: Arc<dyn ItemObserver>) {
        *self.observer.lock_sync() = Some(observer);
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayerItem {
    /// UUID for internal comparisons (queue lookup, etc.).
    pub(crate) fn uuid(&self) -> Uuid {
        self.id
    }

    /// Take the loaded resource for insertion into the player queue.
    pub(crate) fn take_resource(&self) -> FfiResult<Resource> {
        self.resource.lock_sync().take().ok_or(FfiError::NotReady)
    }

    #[expect(dead_code, reason = "used when EventBridge dispatches item events")]
    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock_sync().clone()
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
    fn take_resource_without_load_returns_not_ready() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let result = item.take_resource();
        assert!(matches!(result, Err(FfiError::NotReady)));
    }

    /// Simulates UniFFI's async polling: `load()` is called from a thread
    /// with NO Tokio runtime context. The future must not panic — it should
    /// run I/O on `FFI_RUNTIME` internally.
    #[kithara::test]
    fn load_without_tokio_context_does_not_panic() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let result = std::thread::spawn(move || futures::executor::block_on(item.load()))
            .join()
            .expect("load() panicked — no Tokio runtime context");

        // Network error is expected (example.com won't serve audio).
        // The point is: no panic from missing Tokio reactor.
        assert!(result.is_err());
    }
}
