//! FFI wrapper for the audio player.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use kithara::play::{PlayerConfig, PlayerImpl};
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::{
    event_bridge::EventBridge,
    item::AudioPlayerItem,
    observer::{PlayerObserver, SeekCallback},
    types::{FfiError, FfiItemEvent, FfiPlayerSnapshot, FfiPlayerStatus},
};

/// Entry in the FFI queue. Tracks whether the item has been inserted
/// into the engine's internal queue (after successful resource load).
pub(crate) struct QueueEntry {
    pub(crate) item: Arc<AudioPlayerItem>,
    pub(crate) inserted_into_engine: AtomicBool,
}

/// Count how many entries before `ffi_pos` are inserted into the engine.
/// This maps an FFI queue index to the engine's internal queue index.
fn engine_index(queue: &[Arc<QueueEntry>], ffi_pos: usize) -> usize {
    queue[..ffi_pos]
        .iter()
        .filter(|e| e.inserted_into_engine.load(Ordering::Acquire))
        .count()
}

/// FFI-facing audio player with UUID-based queue management.
#[cfg_attr(feature = "backend-uniffi", derive(uniffi::Object))]
pub struct AudioPlayer {
    inner: Arc<Mutex<PlayerImpl>>,
    queue: Arc<Mutex<Vec<Arc<QueueEntry>>>>,
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
            queue: Arc::new(Mutex::new(Vec::new())),
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

    /// Seek to a position in the current item.
    ///
    /// The callback is invoked synchronously with `true` if the seek command
    /// was accepted, `false` otherwise (matches `AVPlayer` semantics).
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
    pub fn seek(&self, to_seconds: f64, callback: Arc<dyn SeekCallback>) {
        match self.inner.lock_sync().seek_seconds(to_seconds) {
            Ok(()) => callback.on_complete(true),
            Err(_) => callback.on_complete(false),
        }
    }

    /// Insert an item into the queue.
    ///
    /// If the item's resource is already loaded, it is inserted into both
    /// the FFI queue and the engine immediately. If not yet loaded,
    /// the item is placed in the FFI queue and an async auto-load task
    /// is spawned; upon completion the resource is inserted into the engine.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if `after` is not in the queue.
    #[expect(clippy::needless_pass_by_value, reason = "UniFFI requires owned Arc")]
    pub fn insert(
        self: &Arc<Self>,
        item: Arc<AudioPlayerItem>,
        after: Option<Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let mut queue = self.queue.lock_sync();

        let after_ffi_index = after
            .as_ref()
            .map(|after_item| {
                queue
                    .iter()
                    .position(|e| e.item.uuid() == after_item.uuid())
                    .ok_or_else(|| FfiError::InvalidArgument {
                        reason: format!("item {} not found in queue", after_item.id()),
                    })
            })
            .transpose()?;

        let ffi_pos = after_ffi_index.map_or(queue.len(), |i| i + 1);

        let entry = Arc::new(QueueEntry {
            item: Arc::clone(&item),
            inserted_into_engine: AtomicBool::new(false),
        });

        match item.take_resource() {
            Ok(resource) => {
                let eng_idx = engine_index(&queue, ffi_pos);
                self.inner.lock_sync().insert(resource, Some(eng_idx));
                entry.inserted_into_engine.store(true, Ordering::Release);
                queue.insert(ffi_pos, entry);
            }
            Err(FfiError::NotReady) => {
                queue.insert(ffi_pos, Arc::clone(&entry));
                drop(queue);
                self.spawn_auto_load(entry);
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    /// # Errors
    ///
    /// Returns [`FfiError::InvalidArgument`] if the item is not in the queue.
    pub fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        let mut queue = self.queue.lock_sync();
        let ffi_idx = queue
            .iter()
            .position(|e| e.item.uuid() == item.uuid())
            .ok_or_else(|| FfiError::InvalidArgument {
                reason: format!("item {} not found in queue", item.id()),
            })?;

        let entry = &queue[ffi_idx];
        if entry.inserted_into_engine.load(Ordering::Acquire) {
            let eng_idx = engine_index(&queue, ffi_idx);
            self.inner.lock_sync().remove_at(eng_idx);
        }

        queue.remove(ffi_idx);
        drop(queue);
        Ok(())
    }

    pub fn remove_all_items(&self) {
        let mut queue = self.queue.lock_sync();
        self.inner.lock_sync().remove_all_items();
        queue.clear();
    }

    pub fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        self.queue
            .lock_sync()
            .iter()
            .map(|e| Arc::clone(&e.item))
            .collect()
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

    pub fn volume(&self) -> f32 {
        self.inner.lock_sync().volume()
    }

    pub fn set_volume(&self, volume: f32) {
        self.inner.lock_sync().set_volume(volume);
    }

    pub fn is_muted(&self) -> bool {
        self.inner.lock_sync().is_muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.inner.lock_sync().set_muted(muted);
    }

    /// Return a snapshot of the player's current state.
    ///
    /// Cheap synchronous read — Swift should use this instead of caching
    /// state locally.
    #[must_use]
    pub fn snapshot(&self) -> FfiPlayerSnapshot {
        let inner = self.inner.lock_sync();
        FfiPlayerSnapshot {
            status: FfiPlayerStatus::from(inner.status()),
            current_time: inner.position_seconds(),
            duration: inner.duration_seconds(),
            rate: inner.rate(),
            default_rate: inner.default_rate(),
            volume: inner.volume(),
            muted: inner.is_muted(),
        }
    }

    pub fn set_observer(self: &Arc<Self>, observer: Arc<dyn PlayerObserver>) {
        let rx = self.inner.lock_sync().subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Arc::clone(&observer),
            Arc::clone(&self.inner),
            Arc::clone(&self.queue),
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

    /// Spawn an async task that waits for the item to load, then inserts
    /// its resource into the engine.
    ///
    /// Uses `Arc::ptr_eq` to identify the entry — protects against
    /// remove + re-insert race conditions.
    fn spawn_auto_load(&self, entry: Arc<QueueEntry>) {
        let inner = Arc::clone(&self.inner);
        let queue = Arc::clone(&self.queue);

        crate::FFI_RUNTIME.spawn(async move {
            // Wait for the item's load() to finish.
            entry.item.wait_for_load().await;

            let resource = match entry.item.take_resource() {
                Ok(r) => r,
                Err(e) => {
                    error!(%e, item_id = %entry.item.id(), "auto-load failed to take resource");
                    // Remove from FFI queue on failure.
                    {
                        let mut q = queue.lock_sync();
                        if let Some(pos) = q.iter().position(|e2| Arc::ptr_eq(e2, &entry)) {
                            q.remove(pos);
                        }
                    }
                    if let Some(obs) = entry.item.observer() {
                        obs.on_event(FfiItemEvent::Error {
                            error: e.to_string(),
                        });
                    }
                    return;
                }
            };

            // Lock queue → inner (consistent ordering with insert/remove).
            let q = queue.lock_sync();
            let Some(ffi_pos) = q.iter().position(|e2| Arc::ptr_eq(e2, &entry)) else {
                debug!(
                    item_id = %entry.item.id(),
                    "auto-load: entry removed from queue before resource was ready"
                );
                return;
            };

            let eng_idx = engine_index(&q, ffi_pos);
            let player = inner.lock_sync();
            player.insert(resource, Some(eng_idx));
            entry.inserted_into_engine.store(true, Ordering::Release);
            player.try_load_if_current(eng_idx);
            drop(player);
            drop(q);
        });
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

    #[kithara::test]
    fn engine_index_empty() {
        let queue: Vec<Arc<QueueEntry>> = vec![];
        assert_eq!(engine_index(&queue, 0), 0);
    }

    #[kithara::test]
    fn engine_index_all_inserted() {
        let entries: Vec<Arc<QueueEntry>> = (0..3)
            .map(|_| {
                Arc::new(QueueEntry {
                    item: AudioPlayerItem::new("https://example.com/a.mp3".into(), None),
                    inserted_into_engine: AtomicBool::new(true),
                })
            })
            .collect();
        assert_eq!(engine_index(&entries, 0), 0);
        assert_eq!(engine_index(&entries, 1), 1);
        assert_eq!(engine_index(&entries, 2), 2);
        assert_eq!(engine_index(&entries, 3), 3);
    }

    #[kithara::test]
    fn engine_index_mixed() {
        let make = |inserted: bool| {
            Arc::new(QueueEntry {
                item: AudioPlayerItem::new("https://example.com/a.mp3".into(), None),
                inserted_into_engine: AtomicBool::new(inserted),
            })
        };
        // [inserted, NOT inserted, inserted]
        let entries = vec![make(true), make(false), make(true)];
        assert_eq!(engine_index(&entries, 0), 0); // nothing before pos 0
        assert_eq!(engine_index(&entries, 1), 1); // 1 inserted before pos 1
        assert_eq!(engine_index(&entries, 2), 1); // still 1 (pos 1 not inserted)
        assert_eq!(engine_index(&entries, 3), 2); // 2 inserted total
    }

    #[kithara::test]
    fn engine_index_none_inserted() {
        let entries: Vec<Arc<QueueEntry>> = (0..3)
            .map(|_| {
                Arc::new(QueueEntry {
                    item: AudioPlayerItem::new("https://example.com/a.mp3".into(), None),
                    inserted_into_engine: AtomicBool::new(false),
                })
            })
            .collect();
        assert_eq!(engine_index(&entries, 0), 0);
        assert_eq!(engine_index(&entries, 1), 0);
        assert_eq!(engine_index(&entries, 2), 0);
        assert_eq!(engine_index(&entries, 3), 0);
    }

    #[kithara::test]
    fn volume_roundtrip() {
        let player = AudioPlayer::new();
        assert!((player.volume() - 1.0).abs() < f32::EPSILON);
        player.set_volume(0.5);
        assert!((player.volume() - 0.5).abs() < f32::EPSILON);
    }

    #[kithara::test]
    fn muted_roundtrip() {
        let player = AudioPlayer::new();
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
    }
}
