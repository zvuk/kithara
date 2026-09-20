use kithara_platform::sync::Arc;
use kithara_warp::RenderSnapshot;

use crate::{
    api::{SlotId, TrackId},
    bridge::{PlaybackShared, SlotControl},
};

pub(super) struct SlotTable {
    slots: Vec<(SlotId, SlotControl)>,
}

impl SlotTable {
    pub(super) fn contains(&self, slot: SlotId) -> bool {
        self.slots.iter().any(|(id, _)| *id == slot)
    }

    pub(super) fn get(&self, slot: SlotId) -> Option<&SlotControl> {
        self.slots
            .iter()
            .find_map(|(id, control)| (*id == slot).then_some(control))
    }

    pub(super) fn get_mut(&mut self, slot: SlotId) -> Option<&mut SlotControl> {
        self.slots
            .iter_mut()
            .find_map(|(id, control)| (*id == slot).then_some(control))
    }

    pub(super) fn render_snapshot_for(
        &self,
        slot: SlotId,
        item_id: TrackId,
        warp_map: Option<kithara_warp::WarpMapRevision>,
    ) -> Option<RenderSnapshot> {
        self.get(slot)
            .and_then(|control| control.render_snapshot_for(item_id, warp_map))
    }

    pub(super) fn ids(&self) -> Vec<SlotId> {
        self.slots.iter().map(|(id, _)| *id).collect()
    }

    pub(super) fn insert(&mut self, slot: SlotId, control: SlotControl) {
        if let Some((_, existing)) = self.slots.iter_mut().find(|(id, _)| *id == slot) {
            *existing = control;
            return;
        }
        self.slots.push((slot, control));
    }

    pub(super) fn remove(&mut self, slot: SlotId) -> Option<SlotControl> {
        let idx = self.slots.iter().position(|(id, _)| *id == slot)?;
        let mut control = self.slots.remove(idx).1;
        control.cancel_all_scheduled_seeks();
        Some(control)
    }

    pub(super) fn service_scheduled_seeks(&mut self, lead: std::num::NonZeroUsize) {
        for (_, control) in &mut self.slots {
            control.service_scheduled_seeks(lead);
        }
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
        }
    }

    pub(super) fn clear(&mut self) {
        for (_, control) in &mut self.slots {
            control.cancel_all_scheduled_seeks();
        }
        self.slots.clear();
    }

    delegate::delegate! {
        to self.slots {
            pub(super) const fn len(&self) -> usize;
        }
        to self {
            #[expr($.map(|control| Arc::clone(&control.playback)))]
            #[call(get)]
            pub(super) fn playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>>;
            #[expr($.and_then(SlotControl::latest_render_snapshot))]
            #[call(get)]
            pub(super) fn render_snapshot(&self, slot: SlotId) -> Option<RenderSnapshot>;
        }
    }
}
