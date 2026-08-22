use kithara_platform::sync::Arc;

use crate::resource::Resource;

/// A queued resource plus its optional queue-item identity.
pub(crate) struct QueuedResource {
    pub(crate) item_id: Option<Arc<str>>,
    pub(crate) resource: Resource,
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Playlist {
    last_announced: Option<usize>,
    items: Vec<Option<QueuedResource>>,
    #[field(get, set, vis = "pub(crate)")]
    current: usize,
}

impl Playlist {
    pub(crate) const fn advance(&mut self) -> Option<usize> {
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

    delegate::delegate! {
        to self.items {
            #[expr($.is_some_and(Option::is_some))]
            #[call(get)]
            pub(crate) fn has_resource(&self, index: usize) -> bool;
            pub(crate) const fn len(&self) -> usize;
            #[expr($.and_then(Option::take))]
            #[call(get_mut)]
            pub(crate) fn take(&mut self, index: usize) -> Option<QueuedResource>;
        }
    }

    pub(crate) fn insert(&mut self, q: QueuedResource, at: Option<usize>) -> usize {
        let pos = at.map_or(self.items.len(), |i| i.min(self.items.len()));
        self.items.insert(pos, Some(q));
        pos
    }

    pub(crate) fn is_announced(&self, index: usize) -> bool {
        self.last_announced == Some(index)
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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::Playlist;

    #[kithara::test(native, flash(false))]
    fn remove_at_shifts_current_and_reopens_announce() {
        let mut playlist = Playlist::default();
        playlist.reserve(3);
        playlist.set_current(2);
        assert!(playlist.mark_announced(2));

        assert!(playlist.remove_at(0).is_none());

        assert_eq!(playlist.current(), 1);
        assert!(playlist.mark_announced(1));
    }

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
    fn advance_stops_at_end() {
        let mut playlist = Playlist::default();
        playlist.reserve(2);

        assert_eq!(playlist.advance(), Some(1));
        assert_eq!(playlist.advance(), None);
        assert_eq!(playlist.current(), 1);
    }

    #[kithara::test(native, flash(false))]
    fn mark_announced_uses_swap_semantics() {
        let mut playlist = Playlist::default();

        assert!(playlist.mark_announced(0));
        assert!(!playlist.mark_announced(0));
        assert!(playlist.mark_announced(1));
    }
}
