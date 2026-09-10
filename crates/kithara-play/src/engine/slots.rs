use kithara_platform::sync::Arc;
use kithara_warp::RenderSnapshot;

use crate::{
    api::SlotId,
    bridge::{PlaybackShared, SlotControl},
    sync::DeckGrid,
};

pub(super) struct SlotTable {
    grid: DeckGrid,
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

    pub(super) fn ids(&self) -> Vec<SlotId> {
        self.slots.iter().map(|(id, _)| *id).collect()
    }

    pub(super) fn insert(&mut self, slot: SlotId, mut control: SlotControl) {
        control.grid.write(self.grid);
        if let Some((_, existing)) = self.slots.iter_mut().find(|(id, _)| *id == slot) {
            *existing = control;
            return;
        }
        self.slots.push((slot, control));
    }

    pub(super) fn publish_deck_grid(&mut self, grid: DeckGrid) {
        self.grid = grid;
        for (_, control) in &mut self.slots {
            control.grid.write(grid);
        }
    }

    pub(super) fn remove(&mut self, slot: SlotId) -> Option<SlotControl> {
        let idx = self.slots.iter().position(|(id, _)| *id == slot)?;
        Some(self.slots.remove(idx).1)
    }

    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            grid: DeckGrid::default(),
        }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use kithara_warp::{SessionAnchor, SessionBeat, SessionFrame};

    use super::SlotTable;
    use crate::{
        api::SlotId,
        bridge::{SharedEq, slot_channels},
        sync::DeckGrid,
    };

    #[kithara::test]
    fn grid_updates_reach_existing_and_future_slots() {
        let mut slots = SlotTable::with_capacity(2);
        let (mut first, control) = slot_channels(SharedEq::new(0));
        slots.insert(SlotId::new(0), control);
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::default(),
            1.5,
            NonZeroU32::new(48_000).expect("sample rate"),
        )
        .expect("anchor");
        slots.publish_deck_grid(DeckGrid::Local(anchor));
        let (mut next, control) = slot_channels(SharedEq::new(0));
        slots.insert(SlotId::new(1), control);
        for grid in [*first.grid.read(), *next.grid.read()] {
            assert!(matches!(grid, DeckGrid::Local(actual) if actual == anchor));
        }
    }
}
