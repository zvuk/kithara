use std::{
    collections::VecDeque,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara_platform::sync::{Mutex, MutexGuard};

use crate::segment::PlannedFetch;

/// The fetch plan with a lock-free membership mirror.
///
/// The deque is the plan's order; the planner side mutates it under the
/// mutex. `HlsVariant::fetch_is_planned` runs on the produce core inside
/// `phase_at`, and a blocking lock there spins into `sched_yield` under
/// planner contention (`RTSan`: unsafe-library-call in real-time context).
/// Membership is therefore mirrored into atomics, updated by every mutation
/// while it still holds the queue lock, and the produce core reads only the
/// mirror.
pub(in crate::variant) struct PlanQueue {
    queue: Mutex<VecDeque<PlannedFetch>>,
    init_planned: AtomicBool,
    segments_planned: Box<[AtomicBool]>,
}

impl PlanQueue {
    pub(in crate::variant) fn new(queue_capacity: usize, num_segments: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(queue_capacity)),
            init_planned: AtomicBool::new(false),
            segments_planned: (0..num_segments).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    /// Lock-free membership probe for the produce core. May observe a
    /// mutation slightly early or late relative to the deque — the same
    /// temporal slack a racing lock acquisition always had.
    pub(in crate::variant) fn planned(&self, planned: PlannedFetch) -> bool {
        match planned {
            PlannedFetch::Init => self.init_planned.load(Ordering::Relaxed),
            PlannedFetch::Segment(idx) => self
                .segments_planned
                .get(idx as usize)
                .is_some_and(|flag| flag.load(Ordering::Relaxed)),
        }
    }

    pub(in crate::variant) fn lock(&self) -> PlanGuard<'_> {
        PlanGuard {
            queue: self.queue.lock(),
            init_planned: &self.init_planned,
            segments_planned: &self.segments_planned,
        }
    }

    fn mark(
        init_planned: &AtomicBool,
        segments_planned: &[AtomicBool],
        planned: PlannedFetch,
        value: bool,
    ) {
        match planned {
            PlannedFetch::Init => init_planned.store(value, Ordering::Relaxed),
            PlannedFetch::Segment(idx) => {
                if let Some(flag) = segments_planned.get(idx as usize) {
                    flag.store(value, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Read access is `Deref` to the deque; mutation goes through the guard's
/// own methods so the membership mirror can never drift from the plan.
pub(in crate::variant) struct PlanGuard<'a> {
    queue: MutexGuard<'a, VecDeque<PlannedFetch>>,
    init_planned: &'a AtomicBool,
    segments_planned: &'a [AtomicBool],
}

impl Deref for PlanGuard<'_> {
    type Target = VecDeque<PlannedFetch>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl PlanGuard<'_> {
    pub(in crate::variant) fn pop_front(&mut self) -> Option<PlannedFetch> {
        let planned = self.queue.pop_front();
        if let Some(planned) = planned {
            PlanQueue::mark(self.init_planned, self.segments_planned, planned, false);
        }
        planned
    }

    pub(in crate::variant) fn push_front(&mut self, planned: PlannedFetch) {
        self.queue.push_front(planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    /// Insert keeping plan order (`Init` first, segments ascending). The
    /// queue is built sorted and every mutation preserves that, so a single
    /// ordered insert is enough.
    pub(in crate::variant) fn insert_sorted(&mut self, planned: PlannedFetch) {
        let at = self
            .queue
            .iter()
            .position(|queued| *queued > planned)
            .unwrap_or(self.queue.len());
        self.queue.insert(at, planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    #[cfg(test)]
    pub(in crate::variant) fn push_back(&mut self, planned: PlannedFetch) {
        self.queue.push_back(planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    #[cfg(test)]
    pub(in crate::variant) fn clear(&mut self) {
        self.replace_with(std::iter::empty());
    }

    pub(in crate::variant) fn replace_with(&mut self, plan: impl Iterator<Item = PlannedFetch>) {
        self.queue.clear();
        self.init_planned.store(false, Ordering::Relaxed);
        for flag in self.segments_planned {
            flag.store(false, Ordering::Relaxed);
        }
        for planned in plan {
            self.queue.push_back(planned);
            PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> PlanQueue {
        PlanQueue::new(8, 4)
    }

    #[test]
    fn a_replaced_plan_is_visible_without_the_lock() {
        let queue = plan();

        queue
            .lock()
            .replace_with([PlannedFetch::Init, PlannedFetch::Segment(2)].into_iter());

        assert!(queue.planned(PlannedFetch::Init));
        assert!(queue.planned(PlannedFetch::Segment(2)));
        assert!(!queue.planned(PlannedFetch::Segment(1)));
    }

    #[test]
    fn a_popped_fetch_leaves_the_mirror() {
        let queue = plan();
        queue
            .lock()
            .replace_with([PlannedFetch::Segment(1)].into_iter());

        assert_eq!(queue.lock().pop_front(), Some(PlannedFetch::Segment(1)));

        assert!(!queue.planned(PlannedFetch::Segment(1)));
    }

    #[test]
    fn a_requeued_fetch_reenters_the_mirror() {
        let queue = plan();

        queue.lock().push_front(PlannedFetch::Segment(3));

        assert!(queue.planned(PlannedFetch::Segment(3)));
    }

    #[test]
    fn a_segment_outside_the_map_is_never_planned() {
        let queue = plan();

        queue.lock().push_front(PlannedFetch::Segment(40));

        assert!(!queue.planned(PlannedFetch::Segment(40)));
        assert_eq!(queue.lock().pop_front(), Some(PlannedFetch::Segment(40)));
    }
}
