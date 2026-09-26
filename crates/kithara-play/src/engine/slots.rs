use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_sync::LoadGeneration;
use kithara_warp::RenderSnapshot;

use crate::{
    api::SlotId,
    bridge::{PlaybackShared, SlotControl},
};

pub(super) struct SlotTable {
    slots: Vec<(SlotId, SlotEntry)>,
}

pub(super) struct SlotEntry {
    pub(super) control: SlotControl,
    pub(super) reserved_cmds: usize,
    pub(super) closing: bool,
}

impl SlotTable {
    pub(super) fn get(&self, slot: SlotId) -> Option<&SlotControl> {
        self.slots
            .iter()
            .find_map(|(id, entry)| (*id == slot).then_some(&entry.control))
    }

    delegate::delegate! {
        to self.slots {
            pub(super) fn clear(&mut self);
            pub(super) const fn len(&self) -> usize;
        }
        to self {
            #[expr($.map(|control| Arc::clone(&control.playback)))]
            #[call(get)]
            pub(super) fn playback(&self, slot: SlotId) -> Option<Arc<PlaybackShared>>;
            #[expr($.and_then(SlotControl::latest_render_snapshot))]
            #[call(get)]
            pub(super) fn render_snapshot(&self, slot: SlotId) -> Option<RenderSnapshot>;
            #[expr($.map(|entry| &mut entry.control))]
            #[call(entry_mut)]
            pub(super) fn get_mut(&mut self, slot: SlotId) -> Option<&mut SlotControl>;
        }
    }

    pub(super) fn entry_mut(&mut self, slot: SlotId) -> Option<&mut SlotEntry> {
        self.slots
            .iter_mut()
            .find_map(|(id, entry)| (*id == slot).then_some(entry))
    }

    pub(super) fn render_binding(
        &self,
        slot: SlotId,
        item_id: TrackId,
    ) -> Option<(LoadGeneration, Option<RenderSnapshot>)> {
        self.get(slot)?.render_binding(item_id)
    }

    pub(super) fn ids(&self) -> Vec<SlotId> {
        self.slots.iter().map(|(id, _)| *id).collect()
    }

    /// Exclude new producers before the engine detaches its slots from Host.
    /// A load already holding command capacity keeps every slot alive.
    pub(super) fn begin_close_all(&mut self) -> Result<(), SlotId> {
        if let Some((slot, _)) = self
            .slots
            .iter()
            .find(|(_, entry)| entry.reserved_cmds != 0 || entry.closing)
        {
            return Err(*slot);
        }
        for (_, entry) in &mut self.slots {
            entry.closing = true;
        }
        Ok(())
    }

    pub(super) fn abort_close_all(&mut self) {
        for (_, entry) in &mut self.slots {
            entry.closing = false;
        }
    }

    pub(super) fn insert(&mut self, slot: SlotId, control: SlotControl) {
        let entry = SlotEntry {
            control,
            reserved_cmds: 0,
            closing: false,
        };
        if let Some((_, existing)) = self.slots.iter_mut().find(|(id, _)| *id == slot) {
            *existing = entry;
            return;
        }
        self.slots.push((slot, entry));
    }

    pub(super) fn remove(&mut self, slot: SlotId) -> Option<SlotControl> {
        let idx = self.slots.iter().position(|(id, _)| *id == slot)?;
        Some(self.slots.remove(idx).1.control)
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
        }
    }
}
