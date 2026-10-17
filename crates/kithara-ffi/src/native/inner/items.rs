use kithara::{platform::sync::Arc, queue::QueueError};

use super::NativeInner;
use crate::{item::AudioPlayerItem, types::FfiError};
impl NativeInner {
    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let source = self.resources.source(item, self.queue.bus())?;
        let id = item.track_id();
        self.enqueue(item, || {
            self.queue
                .append_with_id(id, source)
                .map(|_| ())
                .map_err(|error| FfiError::Internal {
                    description: error.to_string(),
                })
        })
    }

    /// Registers `item` before `add` puts it into the queue: a load that
    /// fails at once reports its status while `add` is still returning, and
    /// the event bridge drops a status for an item it cannot find.
    fn enqueue(
        &self,
        item: &Arc<AudioPlayerItem>,
        add: impl FnOnce() -> Result<(), FfiError>,
    ) -> Result<(), FfiError> {
        let id = item.track_id();
        self.items.lock().insert(id, Arc::clone(item));
        if let Err(error) = add() {
            self.items.lock().remove(&id);
            return Err(error);
        }
        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let entry = self.queue.current()?;
        self.items.lock().get(&entry.id).cloned()
    }

    pub(crate) fn insert(
        &self,
        item: &Arc<AudioPlayerItem>,
        after: Option<&Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let source = self.resources.source(item, self.queue.bus())?;
        let id = item.track_id();
        let after_id = after.map(|i| i.track_id());

        self.enqueue(item, || {
            self.queue
                .insert_with_id(id, source, after_id)
                .map(|_| ())
                .map_err(|e| FfiError::InvalidArgument {
                    reason: e.to_string(),
                })
        })
    }

    pub(crate) fn item_count(&self) -> u32 {
        let len = self.queue.len();
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        let tracks = self.queue.tracks();
        let items = self.items.lock();
        tracks
            .iter()
            .filter_map(|t| items.get(&t.id).cloned())
            .collect()
    }

    pub(crate) fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        if !*item.inserted.lock() {
            return Err(FfiError::InvalidArgument {
                reason: format!("item {} not in queue", item.audio_id()),
            });
        }
        let id = item.track_id();
        self.queue
            .remove(id)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;
        self.items.lock().remove(&id);
        *item.inserted.lock() = false;
        Ok(())
    }

    pub(crate) fn remove_all_items(&self) {
        if let Err(error) = self.queue.clear() {
            tracing::warn!(%error, "the queue refused to clear and keeps its items");
            return;
        }
        let mut items = self.items.lock();
        for (_, item) in items.drain() {
            *item.inserted.lock() = false;
        }
    }

    pub(crate) fn replace_item(
        &self,
        index: u32,
        item: &Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let idx = index as usize;
        let tracks = self.queue.tracks();
        let old = tracks.get(idx).ok_or_else(|| FfiError::InvalidArgument {
            reason: format!("item index {idx} out of range (len: {})", tracks.len()),
        })?;
        let old_id = old.id;

        let source = self.resources.source(item, self.queue.bus())?;
        let after_for_insert = if idx == 0 {
            None
        } else {
            tracks.get(idx - 1).map(|e| e.id)
        };
        let new_id = item.track_id();
        self.queue
            .insert_with_id(new_id, source, after_for_insert)
            .map_err(|e| FfiError::InvalidArgument {
                reason: e.to_string(),
            })?;
        let _ = self.queue.remove(old_id);

        {
            let mut items = self.items.lock();
            items.remove(&old_id);
            items.insert(new_id, Arc::clone(item));
        }
        *item.inserted.lock() = true;
        item.restart_bridge();
        Ok(())
    }

    pub(crate) fn select(
        &self,
        item: &AudioPlayerItem,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        self.queue
            .select(item.track_id(), transition.try_into()?)
            .map_err(|e| match e {
                QueueError::NotReady(_) => FfiError::NotReady,
                other => FfiError::Internal {
                    description: other.to_string(),
                },
            })
    }
}
