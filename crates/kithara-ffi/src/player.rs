//! FFI wrapper for the audio player.

use std::sync::Arc;

use kithara::play::{PlayerConfig, PlayerImpl};
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    event_bridge::EventBridge, item::AudioPlayerItem, observer::PlayerObserver, types::FfiError,
};

/// FFI-facing audio player with UUID-based queue management.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayer {
    inner: Arc<Mutex<PlayerImpl>>,
    queue: Mutex<Vec<Arc<AudioPlayerItem>>>,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    event_bridge: Mutex<Option<EventBridge>>,
}

/// Methods exported across the FFI boundary.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
impl AudioPlayer {
    #[must_use]
    #[cfg_attr(feature = "backend-uniffi", uniffi::constructor)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Mutex::new(PlayerImpl::new(PlayerConfig::default()))),
            queue: Mutex::new(Vec::new()),
            observer: Mutex::new(None),
            event_bridge: Mutex::new(None),
        })
    }

    pub fn play(&self) {
        self.inner.lock_sync().play();
    }

    pub fn pause(&self) {
        self.inner.lock_sync().pause();
    }

    /// # Errors
    ///
    /// Returns [`FfiError`] if the seek position is invalid.
    pub fn seek(&self, to_seconds: f64) -> Result<(), FfiError> {
        self.inner
            .lock_sync()
            .seek_seconds(to_seconds)
            .map_err(FfiError::from)
    }

    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `after` is not in the queue,
    /// or [`FfiError::NotReady`] if the item has not been loaded.
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
    pub fn insert(
        &self,
        item: Arc<AudioPlayerItem>,
        after: Option<Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        // Lock queue first, validate `after`, then take_resource to avoid
        // consuming it before validation succeeds.
        let mut queue = self.queue.lock_sync();

        let after_index = after
            .as_ref()
            .map(|after_item| {
                queue
                    .iter()
                    .position(|i| i.uuid() == after_item.uuid())
                    .ok_or_else(|| FfiError::InvalidArgument {
                        reason: format!("item {} not found in queue", after_item.id()),
                    })
            })
            .transpose()?;

        let resource = item.take_resource()?;

        // Hold both locks to keep inner and queue in sync.
        self.inner.lock_sync().insert(resource, after_index);

        match after_index {
            Some(idx) => queue.insert(idx + 1, item),
            None => queue.push(item),
        }
        drop(queue);

        Ok(())
    }

    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if the item is not in the queue.
    pub fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        // Hold both locks to keep inner and queue in sync.
        let mut queue = self.queue.lock_sync();
        let idx = queue
            .iter()
            .position(|i| i.uuid() == item.uuid())
            .ok_or_else(|| FfiError::InvalidArgument {
                reason: format!("item {} not found in queue", item.id()),
            })?;

        self.inner.lock_sync().remove_at(idx);

        queue.remove(idx);
        drop(queue);
        Ok(())
    }

    pub fn remove_all_items(&self) {
        // Hold both locks to keep inner and queue in sync.
        let mut queue = self.queue.lock_sync();
        self.inner.lock_sync().remove_all_items();
        queue.clear();
    }

    pub fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.queue.lock_sync().clone()
    }

    pub fn default_rate(&self) -> f32 {
        self.inner.lock_sync().default_rate()
    }

    pub fn set_default_rate(&self, rate: f32) {
        self.inner.lock_sync().set_default_rate(rate);
    }

    pub fn rate(&self) -> f32 {
        self.inner.lock_sync().rate()
    }

    pub fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        let rx = self.inner.lock_sync().subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Arc::clone(&observer),
            Arc::clone(&self.inner),
            CancellationToken::new(),
        );

        // Update both atomically: old bridge is dropped (cancelled) only after
        // new one is stored, and observer ref stays in sync with bridge.
        let mut eb = self.event_bridge.lock_sync();
        let mut obs = self.observer.lock_sync();
        *eb = Some(bridge);
        *obs = Some(observer);
        drop(obs);
        drop(eb);
    }
}

/// Internal methods not exported across FFI.
impl AudioPlayer {
    #[expect(dead_code, reason = "reserved for future event bridge extensions")]
    pub(crate) fn observer(&self) -> Option<Arc<dyn PlayerObserver>> {
        self.observer.lock_sync().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn create_player() {
        let _player = AudioPlayer::new();
    }

    #[kithara::test]
    fn default_rate_roundtrip() {
        let player = AudioPlayer::new();
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        player.set_default_rate(0.5);
        assert!((player.default_rate() - 0.5).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn items_initially_empty() {
        let player = AudioPlayer::new();
        assert!(player.items().is_empty());
    }

    #[kithara::test]
    fn remove_all_items_on_empty_queue() {
        let player = AudioPlayer::new();
        player.remove_all_items();
        assert!(player.items().is_empty());
    }
}
