//! FFI wrapper for audio player items.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara::play::{Resource, ResourceConfig};
use kithara_platform::{Mutex, tokio::sync::Notify};
use tracing::error;
use uuid::Uuid;

use crate::{
    observer::ItemObserver,
    types::{FfiError, FfiItemEvent, FfiResult},
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
    loading: AtomicBool,
    load_notify: Notify,
}

/// Internal configuration built from item properties before loading.
struct ResourceLoadConfig {
    config: ResourceConfig,
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
            loading: AtomicBool::new(false),
            load_notify: Notify::new(),
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

    /// Synchronous fire-and-forget load.
    ///
    /// Spawns resource creation on `FFI_RUNTIME`. Errors are reported
    /// through [`ItemObserver::on_event`] instead of being returned.
    /// Double-calls are ignored (idempotent).
    pub fn load(self: Arc<Self>) {
        if !self.claim_loading() {
            return;
        }

        let config = match self.build_resource_config() {
            Ok(c) => c,
            Err(e) => {
                self.report_error(&e);
                self.finish_loading();
                return;
            }
        };

        crate::FFI_RUNTIME.spawn(async move {
            match Self::load_resource(config).await {
                Ok(resource) => {
                    *self.resource.lock_sync() = Some(resource);
                }
                Err(e) => {
                    self.report_error(&e);
                }
            }
            self.finish_loading();
        });
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

    pub(crate) fn observer(&self) -> Option<Arc<dyn ItemObserver>> {
        self.observer.lock_sync().clone()
    }

    /// Atomically claim loading rights. Returns `true` if this call won.
    fn claim_loading(&self) -> bool {
        !self.loading.swap(true, Ordering::AcqRel)
    }

    /// Mark loading as finished and notify any waiters.
    fn finish_loading(&self) {
        self.loading.store(false, Ordering::Release);
        self.load_notify.notify_waiters();
    }

    /// Wait until any in-progress load completes.
    ///
    /// Registers `notified()` BEFORE checking the flag to avoid TOCTOU race.
    pub(crate) async fn wait_for_load(&self) {
        loop {
            let notified = self.load_notify.notified();
            if !self.loading.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Build a [`ResourceConfig`] from item properties.
    fn build_resource_config(&self) -> FfiResult<ResourceLoadConfig> {
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

        Ok(ResourceLoadConfig { config })
    }

    /// Async resource creation — runs on `FFI_RUNTIME`.
    async fn load_resource(load_config: ResourceLoadConfig) -> Result<Resource, FfiError> {
        Resource::new(load_config.config)
            .await
            .map_err(|e| FfiError::Internal {
                message: e.to_string(),
            })
    }

    /// Report an error through the item observer.
    fn report_error(&self, err: &FfiError) {
        error!(%err, item_id = %self.id, "item load failed");
        if let Some(obs) = self.observer() {
            obs.on_event(FfiItemEvent::Error {
                error: err.to_string(),
            });
        }
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

    #[kithara::test]
    fn load_does_not_panic() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        // Sync fire-and-forget — must not panic.
        item.load();
    }

    #[kithara::test]
    fn double_load_is_idempotent() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let item2 = Arc::clone(&item);
        item.load();
        // Second call should be ignored (claim_loading returns false).
        item2.load();
    }

    #[kithara::test]
    fn build_resource_config_valid_url() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        let config = item.build_resource_config();
        assert!(config.is_ok());
    }

    #[kithara::test]
    fn build_resource_config_invalid_url() {
        let item = AudioPlayerItem::new("not a url".into(), None);
        let config = item.build_resource_config();
        assert!(config.is_err());
    }

    #[kithara::test]
    fn build_resource_config_with_bitrate() {
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), None);
        item.set_preferred_peak_bitrate(256_000.0);
        let config = item.build_resource_config();
        assert!(config.is_ok());
    }

    #[kithara::test]
    fn build_resource_config_with_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer token".into());
        let item = AudioPlayerItem::new("https://example.com/a.mp3".into(), Some(headers));
        let config = item.build_resource_config();
        assert!(config.is_ok());
    }
}
