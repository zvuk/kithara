//! Strategies for node traversal order.

use crate::{ServiceClass, SlotId};

/// Metadata about a slot used by the scheduling strategy.
#[derive(Clone, Debug)]
pub struct SlotMeta {
    /// The unique ID of the slot.
    pub id: SlotId,
    /// The current service class (priority) of the slot.
    pub service_class: ServiceClass,
}

/// A strategy that determines the order in which nodes are ticked.
pub trait Schedule: Send + 'static {
    /// Called when slots are added/removed or service class changes.
    /// The strategy should update its internal order based on the new metadata.
    fn reorder(&mut self, slots: &mut [SlotMeta]);

    /// Returns the indices of the slots to tick in the current pass.
    ///
    /// The indices refer to the `slots` slice passed to `reorder`.
    fn next_indices<'a>(&'a mut self, slots: &'a [SlotMeta]) -> &'a [usize];
}

/// A round-robin scheduling strategy.
///
/// Ticks all nodes once per pass, ordered by descending priority (`ServiceClass`).
#[derive(Default)]
pub struct RoundRobin {
    order: Vec<usize>,
}

impl Schedule for RoundRobin {
    // Called only on graph mutations (register / unregister / class change),
    // and `drain_commands` batches multiple commands into a single reorder per
    // pass, so the O(N log N) sort is well off the hot path. For typical N
    // (tens to low hundreds of slots) this is a few microseconds on the
    // scheduler thread; `next_indices` itself is O(1).
    fn reorder(&mut self, slots: &mut [SlotMeta]) {
        self.order.clear();
        self.order.extend(0..slots.len());
        // Sort by descending service class (Audible > Warm > Idle).
        // For equal service classes, preserve the original order (stable sort).
        self.order.sort_by(|&a, &b| {
            let class_a = slots[a].service_class;
            let class_b = slots[b].service_class;
            class_b.cmp(&class_a)
        });
    }

    fn next_indices<'a>(&'a mut self, _slots: &'a [SlotMeta]) -> &'a [usize] {
        &self.order
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn round_robin_sorts_by_service_class() {
        let mut slots = vec![
            SlotMeta {
                id: 1,
                service_class: ServiceClass::Idle,
            },
            SlotMeta {
                id: 2,
                service_class: ServiceClass::Audible,
            },
            SlotMeta {
                id: 3,
                service_class: ServiceClass::Warm,
            },
        ];

        let mut schedule = RoundRobin::default();
        schedule.reorder(&mut slots);

        let indices = schedule.next_indices(&slots);
        assert_eq!(indices, &[1, 2, 0]); // Audible (idx 1), Warm (idx 2), Idle (idx 0)
    }
}
