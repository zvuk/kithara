use std::collections::BTreeMap;

use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_warp::{BeatGridId, BeatGridRevision, RegionPlan, RegionPlanSlot, SegmentSet};

use crate::resource::Resource;

#[derive(Clone, Debug)]
pub(crate) struct TrackGrid {
    pub(crate) id: BeatGridId,
    pub(crate) revision: BeatGridRevision,
    pub(crate) segments: SegmentSet,
}

#[derive(Default)]
struct TrackState {
    slot: Option<Arc<RegionPlanSlot>>,
    plan: Option<Arc<RegionPlan>>,
    grid: Option<TrackGrid>,
}

/// A queued resource plus the queue's identity for it.
pub(crate) struct QueuedResource {
    pub(crate) resource: Resource,
    pub(crate) item_id: TrackId,
}

struct Slot {
    resource: Option<Resource>,
    item_id: TrackId,
}

impl From<QueuedResource> for Slot {
    fn from(queued: QueuedResource) -> Self {
        Self {
            item_id: queued.item_id,
            resource: Some(queued.resource),
        }
    }
}

impl Slot {
    fn take(&mut self) -> Option<QueuedResource> {
        self.resource.take().map(|resource| QueuedResource {
            resource,
            item_id: self.item_id,
        })
    }
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Playlist {
    last_announced: Option<usize>,
    items: Vec<Option<Slot>>,
    tracks: BTreeMap<TrackId, TrackState>,
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
        self.tracks.clear();
        self.current = 0;
        self.last_announced = None;
    }

    pub(crate) fn clear_item(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index)
            && let Some(cleared) = item.take()
        {
            self.tracks.remove(&cleared.item_id);
        }
    }

    pub(crate) fn track_loaded(&mut self, item: TrackId, slot: Option<Arc<RegionPlanSlot>>) {
        let Some(slot) = slot else {
            return;
        };
        let track = self.tracks.entry(item).or_default();
        slot.install(track.plan.clone());
        track.slot = Some(slot);
    }

    pub(crate) fn track_grid(&self, item: TrackId) -> Option<&TrackGrid> {
        self.tracks.get(&item).and_then(|track| track.grid.as_ref())
    }

    pub(crate) fn publish_track_grid(&mut self, item: TrackId, grid: TrackGrid) {
        self.tracks.entry(item).or_default().grid = Some(grid);
    }

    pub(crate) fn set_track_plan(&mut self, item: TrackId, plan: Option<Arc<RegionPlan>>) {
        let track = self.tracks.entry(item).or_default();
        if let Some(slot) = &track.slot {
            slot.install(plan.clone());
        }
        track.plan = plan;
    }

    pub(crate) fn has_resource(&self, index: usize) -> bool {
        self.items
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|slot| slot.resource.is_some())
    }

    pub(crate) fn insert(&mut self, q: QueuedResource, at: Option<usize>) -> usize {
        let pos = at.map_or(self.items.len(), |i| i.min(self.items.len()));
        self.items.insert(pos, Some(q.into()));
        pos
    }

    pub(crate) fn is_announced(&self, index: usize) -> bool {
        self.last_announced == Some(index)
    }

    pub(crate) fn item_id(&self, index: usize) -> Option<TrackId> {
        self.items
            .get(index)
            .and_then(Option::as_ref)
            .map(|queued| queued.item_id)
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

        let removed = self.items.remove(index).and_then(|mut slot| slot.take());
        if let Some(removed) = &removed {
            self.tracks.remove(&removed.item_id);
        }
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
            *slot = Some(q.into());
            if self.last_announced == Some(index) {
                self.last_announced = None;
            }
        }
    }

    pub(crate) fn reserve(&mut self, count: usize) {
        self.items.resize_with(count, || None);
    }

    pub(crate) fn take(&mut self, index: usize) -> Option<QueuedResource> {
        self.items
            .get_mut(index)
            .and_then(Option::as_mut)
            .and_then(Slot::take)
    }

    delegate::delegate! {
        to self.items {
            pub(crate) const fn len(&self) -> usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::TrackId;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;
    use kithara_warp::{GridSegment, RegionPlan, RegionPlanSlot};

    use super::Playlist;

    fn plan() -> Arc<RegionPlan> {
        Arc::new(RegionPlan::new(vec![GridSegment::new(0, 48_000, 1.0)]).expect("fixture plan"))
    }

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

    #[kithara::test(native, flash(false))]
    fn a_plan_stored_before_the_lane_loads_is_installed_on_load() {
        let mut playlist = Playlist::default();
        let item = TrackId(7);
        let plan = plan();
        playlist.set_track_plan(item, Some(plan.clone()));
        let slot = Arc::new(RegionPlanSlot::default());

        playlist.track_loaded(item, Some(slot.clone()));

        assert!(
            slot.load()
                .is_some_and(|installed| Arc::ptr_eq(&installed, &plan))
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_plan_set_after_the_lane_loads_is_installed_at_once() {
        let mut playlist = Playlist::default();
        let item = TrackId(7);
        let slot = Arc::new(RegionPlanSlot::default());
        playlist.track_loaded(item, Some(slot.clone()));
        assert!(slot.load().is_none());

        let plan = plan();
        playlist.set_track_plan(item, Some(plan.clone()));

        assert!(
            slot.load()
                .is_some_and(|installed| Arc::ptr_eq(&installed, &plan))
        );
    }
}
