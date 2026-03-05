//! FFI wrapper for the audio player.

use std::sync::{Arc, Mutex, PoisonError};

use kithara::play::{PlayerConfig, PlayerImpl};

use crate::{
    item::AudioPlayerItem,
    observer::PlayerObserver,
    types::{FfiError, FfiResult},
};

/// FFI-facing audio player with UUID-based queue management.
pub(crate) struct AudioPlayer {
    inner: Mutex<PlayerImpl>,
    queue: Mutex<Vec<Arc<AudioPlayerItem>>>,
    observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
}

impl AudioPlayer {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(PlayerImpl::new(PlayerConfig::default())),
            queue: Mutex::new(Vec::new()),
            observer: Mutex::new(None),
        }
    }

    pub(crate) fn play(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .play();
    }

    pub(crate) fn pause(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pause();
    }

    pub(crate) fn seek(&self, to_seconds: f64) -> FfiResult<()> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seek_seconds(to_seconds)
            .map_err(FfiError::from)
    }

    pub(crate) fn insert(
        &self,
        item: &Arc<AudioPlayerItem>,
        after: Option<&Arc<AudioPlayerItem>>,
    ) -> FfiResult<()> {
        // Lock queue first, validate `after`, then take_resource to avoid
        // consuming it before validation succeeds.
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);

        let after_index = after
            .map(|after_item| {
                queue
                    .iter()
                    .position(|i| i.id() == after_item.id())
                    .ok_or_else(|| FfiError::InvalidArgument {
                        reason: format!("item {} not found in queue", after_item.id()),
                    })
            })
            .transpose()?;

        let resource = item.take_resource()?;

        // Hold both locks to keep inner and queue in sync.
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(resource, after_index);

        match after_index {
            Some(idx) => queue.insert(idx + 1, Arc::clone(item)),
            None => queue.push(Arc::clone(item)),
        }
        drop(queue);

        Ok(())
    }

    pub(crate) fn remove(&self, item: &AudioPlayerItem) -> FfiResult<()> {
        // Hold both locks to keep inner and queue in sync.
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let idx = queue
            .iter()
            .position(|i| i.id() == item.id())
            .ok_or_else(|| FfiError::InvalidArgument {
                reason: format!("item {} not found in queue", item.id()),
            })?;

        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove_at(idx);

        queue.remove(idx);
        drop(queue);
        Ok(())
    }

    pub(crate) fn remove_all_items(&self) {
        // Hold both locks to keep inner and queue in sync.
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove_all_items();
        queue.clear();
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn default_rate(&self) -> f32 {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .default_rate()
    }

    pub(crate) fn set_default_rate(&self, rate: f32) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_default_rate(rate);
    }

    pub(crate) fn rate(&self) -> f32 {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rate()
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = Some(observer);
    }

    pub(crate) fn observer(&self) -> Option<Arc<dyn PlayerObserver>> {
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
    fn create_player() {
        let _player = AudioPlayer::new();
    }

    #[test]
    fn default_rate_roundtrip() {
        let player = AudioPlayer::new();
        assert!((player.default_rate() - 1.0).abs() < f32::EPSILON);
        player.set_default_rate(0.5);
        assert!((player.default_rate() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn items_initially_empty() {
        let player = AudioPlayer::new();
        assert!(player.items().is_empty());
    }

    #[test]
    fn remove_all_items_on_empty_queue() {
        let player = AudioPlayer::new();
        player.remove_all_items();
        assert!(player.items().is_empty());
    }
}
