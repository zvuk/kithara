use std::sync::Arc;

use crate::{
    api::SlotId,
    bridge::{PlaybackShared, SharedEq, SlotControl},
};

pub(super) struct SlotTable {
    slots: Vec<(SlotId, SlotControl)>,
}

impl SlotTable {
    pub(super) fn clear(&mut self) {
        self.slots.clear();
    }

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

    pub(super) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(super) fn playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>> {
        self.get(slot).map(|control| Arc::clone(&control.playback))
    }

    pub(super) fn remove(&mut self, slot: SlotId) -> Option<SlotControl> {
        let idx = self.slots.iter().position(|(id, _)| *id == slot)?;
        Some(self.slots.remove(idx).1)
    }

    pub(super) fn slot_eq(&self, slot: SlotId) -> Option<SharedEq> {
        self.get(slot).map(|control| control.eq.clone())
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
        }
    }
}
