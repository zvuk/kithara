use kithara_platform::sync::Arc;

use super::super::platform::PreparedBindingResource;
use crate::{api::TrackBinding, player::node::StreamShape, resource::Resource};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PreparedBindingStamp {
    pub(crate) shape: StreamShape,
    pub(crate) transport_revision: u64,
}

impl PreparedBindingStamp {
    pub(crate) const fn new(shape: StreamShape, transport_revision: u64) -> Self {
        Self {
            shape,
            transport_revision,
        }
    }
}

/// A queue entry that retains metadata while its resource is checked out by a load transaction.
pub(crate) struct QueuedResource {
    pub(crate) binding: Option<TrackBinding>,
    pub(crate) item_id: Option<Arc<str>>,
    pub(crate) prepared: Option<PreparedBindingResource>,
    pub(crate) resource: Option<Resource>,
}

pub(crate) struct QueuedLoad {
    pub(crate) binding: Option<TrackBinding>,
    pub(crate) item_id: Option<Arc<str>>,
    pub(crate) prepared: Option<PreparedBindingResource>,
    pub(crate) resource: Resource,
}

#[derive(Default)]
pub(crate) struct Playlist {
    items: Vec<Option<QueuedResource>>,
    current: usize,
    last_announced: Option<usize>,
}

impl Playlist {
    pub(crate) fn advance(&mut self) -> Option<usize> {
        let next = self.current + 1;
        if next < self.items.len() {
            self.current = next;
            Some(next)
        } else {
            None
        }
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.current = 0;
        self.last_announced = None;
    }

    pub(crate) fn clear_item(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            *item = None;
        }
    }

    pub(crate) fn current(&self) -> usize {
        self.current
    }

    pub(crate) fn current_binding(&self) -> Option<&TrackBinding> {
        self.get(self.current)
            .and_then(|item| item.binding.as_ref())
    }

    pub(crate) fn has_resource(&self, index: usize) -> bool {
        self.items
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|queued| queued.resource.is_some())
    }

    pub(crate) fn get(&self, index: usize) -> Option<&QueuedResource> {
        self.items.get(index).and_then(Option::as_ref)
    }

    pub(crate) fn insert(&mut self, q: QueuedResource, at: Option<usize>) -> usize {
        let pos = at.map_or(self.items.len(), |i| i.min(self.items.len()));
        self.items.insert(pos, Some(q));
        pos
    }

    pub(crate) fn is_announced(&self, index: usize) -> bool {
        self.last_announced == Some(index)
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn mark_announced(&mut self, index: usize) -> bool {
        let changed = self.last_announced != Some(index);
        self.last_announced = Some(index);
        changed
    }

    pub(crate) fn remove_at(&mut self, index: usize) -> Option<QueuedResource> {
        if index >= self.items.len() {
            return None;
        }

        let removed = self.items.remove(index);
        if index < self.current {
            self.current = self.current.saturating_sub(1);
        } else if index == self.current
            && self.current >= self.items.len()
            && !self.items.is_empty()
        {
            self.current = self.items.len() - 1;
        }
        self.last_announced = None;
        removed
    }

    pub(crate) fn replace(&mut self, index: usize, q: QueuedResource) {
        if let Some(slot) = self.items.get_mut(index) {
            *slot = Some(q);
            if self.last_announced == Some(index) {
                self.last_announced = None;
            }
        }
    }

    pub(crate) fn reserve(&mut self, count: usize) {
        self.items.resize_with(count, || None);
    }

    pub(crate) fn set_current(&mut self, index: usize) {
        self.current = index;
    }

    pub(crate) fn take(&mut self, index: usize) -> Option<QueuedLoad> {
        let queued = self.items.get_mut(index)?.as_mut()?;
        let resource = queued.resource.take()?;
        Some(QueuedLoad {
            binding: queued.binding.clone(),
            item_id: queued.item_id.clone(),
            prepared: queued.prepared.take(),
            resource,
        })
    }

    pub(crate) fn restore(
        &mut self,
        index: usize,
        prepared: Option<PreparedBindingResource>,
        resource: Resource,
    ) -> bool {
        let Some(queued) = self.items.get_mut(index).and_then(Option::as_mut) else {
            return false;
        };
        if queued.resource.is_some() {
            return false;
        }
        queued.prepared = prepared;
        queued.resource = Some(resource);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::Playlist;
    use crate::player::{node::StreamShape, state::PreparedBindingStamp};

    fn stamp(
        sample_rate: u32,
        max_block_frames: u32,
        transport_revision: u64,
    ) -> PreparedBindingStamp {
        PreparedBindingStamp::new(
            StreamShape {
                sample_rate: NonZeroU32::new(sample_rate).expect("static sample rate"),
                max_block_frames: NonZeroU32::new(max_block_frames).expect("static block size"),
            },
            transport_revision,
        )
    }

    #[test]
    fn prepared_binding_stamp_requires_revision_and_full_stream_shape() {
        let prepared = stamp(44_100, 512, 7);

        assert_eq!(prepared, stamp(44_100, 512, 7));
        assert_ne!(prepared, stamp(44_100, 512, 8));
        assert_ne!(prepared, stamp(48_000, 512, 7));
        assert_ne!(prepared, stamp(44_100, 1_024, 7));
    }

    #[test]
    fn remove_at_shifts_current_and_reopens_announce() {
        let mut playlist = Playlist::default();
        playlist.reserve(3);
        playlist.set_current(2);
        assert!(playlist.mark_announced(2));

        assert!(playlist.remove_at(0).is_none());

        assert_eq!(playlist.current(), 1);
        assert!(playlist.mark_announced(1));
    }

    #[test]
    fn clear_resets_cursor_and_announce() {
        let mut playlist = Playlist::default();
        playlist.reserve(2);
        playlist.set_current(1);
        assert!(playlist.mark_announced(1));

        playlist.clear();

        assert_eq!(playlist.current(), 0);
        assert_eq!(playlist.len(), 0);
        assert!(playlist.mark_announced(0));
    }

    #[test]
    fn advance_stops_at_end() {
        let mut playlist = Playlist::default();
        playlist.reserve(2);

        assert_eq!(playlist.advance(), Some(1));
        assert_eq!(playlist.advance(), None);
        assert_eq!(playlist.current(), 1);
    }

    #[test]
    fn mark_announced_uses_swap_semantics() {
        let mut playlist = Playlist::default();

        assert!(playlist.mark_announced(0));
        assert!(!playlist.mark_announced(0));
        assert!(playlist.mark_announced(1));
    }
}
