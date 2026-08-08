use num_traits::cast::AsPrimitive;

use super::flex::Item;
use crate::layout::Axis;

#[derive(Clone, Copy, Default)]
pub(super) struct Allocation {
    extent: f32,
    is_pinned: bool,
}

impl Allocation {
    pub(super) const fn extent(self) -> f32 {
        self.extent
    }
}

pub(super) fn allocate(items: &[Item], axis: Axis, remaining: f32) -> Vec<Allocation> {
    let minimum_sum = items
        .iter()
        .filter(|item| item.main_weight(axis) != 0.0)
        .map(|item| f64::from(minimum(item)))
        .sum::<f64>();
    let mut allocations = vec![Allocation::default(); items.len()];

    if minimum_sum > f64::from(remaining) {
        let scale = if minimum_sum > 0.0 {
            f64::from(remaining) / minimum_sum
        } else {
            0.0
        };

        for (item, allocation) in items.iter().zip(&mut allocations) {
            if item.main_weight(axis) != 0.0 {
                allocation.extent = (f64::from(minimum(item)) * scale).as_();
            }
        }
        return allocations;
    }

    let mut pool = f64::from(remaining);

    loop {
        let weight_sum = remaining_weight(items, &allocations, axis);
        let next =
            items
                .iter()
                .zip(&allocations)
                .enumerate()
                .find_map(|(index, (item, allocation))| {
                    if allocation.is_pinned {
                        return None;
                    }
                    let weight = item.main_weight(axis);
                    if weight == 0.0 {
                        return None;
                    }
                    let item_minimum = minimum(item);
                    let share = pool * f64::from(weight) / weight_sum;
                    (share < f64::from(item_minimum)).then_some((index, item_minimum))
                });
        let Some((index, minimum)) = next else {
            break;
        };

        allocations[index] = Allocation {
            extent: minimum,
            is_pinned: true,
        };
        pool -= f64::from(minimum);
    }

    let weight_sum = remaining_weight(items, &allocations, axis);
    for (item, allocation) in items.iter().zip(&mut allocations) {
        let weight = item.main_weight(axis);
        if weight != 0.0 && !allocation.is_pinned {
            allocation.extent = (pool * f64::from(weight) / weight_sum).as_();
        }
    }

    allocations
}

fn remaining_weight(items: &[Item], allocations: &[Allocation], axis: Axis) -> f64 {
    items
        .iter()
        .zip(allocations)
        .filter(|(_, allocation)| !allocation.is_pinned)
        .map(|(item, _)| f64::from(item.main_weight(axis)))
        .sum()
}

fn minimum(item: &Item) -> f32 {
    item.main_minimum.unwrap_or(0.0).max(0.0)
}
