//! FFI wrapper for audio player items.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use kithara::play::{Resource, ResourceConfig};
use uuid::Uuid;

use crate::{
    observer::ItemObserver,
    types::{FfiError, FfiResult},
};

/// FFI-facing audio player item with UUID identity and lazy loading.
pub(crate) struct AudioPlayerItem {
    id: Uuid,
    url: String,
    headers: Option<HashMap<String, String>>,
    preferred_peak_bitrate: Mutex<f64>,
    preferred_peak_bitrate_expensive: Mutex<f64>,
    resource: Mutex<Option<Resource>>,
    observer: Mutex<Option<Arc<dyn ItemObserver>>>,
}

impl AudioPlayerItem {
    /// Create a new item. Does not start loading — call [`load`] explicitly.
    pub(crate) fn new(
        url: String,
        additional_headers: Option<HashMap<String, String>>,
    ) -> Arc<Self> {
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

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn preferred_peak_bitrate(&self) -> f64 {
        *self
            .preferred_peak_bitrate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_preferred_peak_bitrate(&self, bitrate: f64) {
        *self
            .preferred_peak_bitrate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = bitrate;
    }

    pub(crate) fn preferred_peak_bitrate_for_expensive_networks(&self) -> f64 {
        *self
            .preferred_peak_bitrate_expensive
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_preferred_peak_bitrate_for_expensive_networks(&self, bitrate: f64) {
        *self
            .preferred_peak_bitrate_expensive
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = bitrate;
    }

    /// Asynchronously create the underlying [`Resource`].
    pub(crate) async fn load(&self) -> FfiResult<()> {
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

        let resource = Resource::new(config)
            .await
            .map_err(|e| FfiError::Internal {
                message: e.to_string(),
            })?;

        *self.resource.lock().unwrap_or_else(PoisonError::into_inner) = Some(resource);
        Ok(())
    }

    /// Take the loaded resource for insertion into the player queue.
    pub(crate) fn take_resource(&self) -> FfiResult<Resource> {
        self.resource
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or(FfiError::NotReady)
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn ItemObserver>) {
        *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = Some(observer);
    }

    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_item_has_unique_id() {
        let a = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let b = AudioPlayerItem::new("https://example.com/b.mp3".into(), None);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn url_preserved() {
        let item = AudioPlayerItem::new("https://example.com/song.mp3".into(), None);
        assert_eq!(item.url(), "https://example.com/song.mp3");
    }

    #[test]
    fn preferred_peak_bitrate_roundtrip() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        assert_eq!(item.preferred_peak_bitrate(), 0.0);
        item.set_preferred_peak_bitrate(256_000.0);
        assert_eq!(item.preferred_peak_bitrate(), 256_000.0);
    }

    #[test]
    fn take_resource_without_load_returns_not_ready() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let result = item.take_resource();
        assert!(matches!(result, Err(FfiError::NotReady)));
    }
}
