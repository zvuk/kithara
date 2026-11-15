use std::{
    collections::VecDeque,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_platform::sync::{Mutex, MutexGuard};

use crate::segment::PlannedFetch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlanRevision(u64);

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
    init_planned: AtomicBool,
    revision: AtomicU64,
    segments_planned: Box<[AtomicBool]>,
    queue: Mutex<VecDeque<PlannedFetch>>,
}

impl PlanQueue {
    pub(in crate::variant) fn new(queue_capacity: usize, num_segments: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(queue_capacity)),
            revision: AtomicU64::new(0),
            init_planned: AtomicBool::new(false),
            segments_planned: (0..num_segments).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    pub(in crate::variant) fn lock(&self) -> PlanGuard<'_> {
        PlanGuard {
            queue: self.queue.lock(),
            revision: &self.revision,
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

    #[cfg(test)]
    pub(in crate::variant) fn revision(&self) -> PlanRevision {
        PlanRevision(self.revision.load(Ordering::Relaxed))
    }
}

/// Read access is `Deref` to the deque; mutation goes through the guard's
/// own methods so the membership mirror can never drift from the plan.
pub(in crate::variant) struct PlanGuard<'a> {
    init_planned: &'a AtomicBool,
    revision: &'a AtomicU64,
    segments_planned: &'a [AtomicBool],
    queue: MutexGuard<'a, VecDeque<PlannedFetch>>,
}

impl Deref for PlanGuard<'_> {
    type Target = VecDeque<PlannedFetch>;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl PlanGuard<'_> {
    #[cfg(test)]
    pub(in crate::variant) fn clear(&mut self) {
        self.replace_with(std::iter::empty());
    }

    pub(in crate::variant) fn pop_front(&mut self) -> Option<(PlannedFetch, PlanRevision)> {
        let planned = self.queue.pop_front();
        if let Some(planned) = planned {
            PlanQueue::mark(self.init_planned, self.segments_planned, planned, false);
        }
        planned.map(|planned| (planned, PlanRevision(self.revision.load(Ordering::Relaxed))))
    }

    #[cfg(test)]
    pub(in crate::variant) fn push_back(&mut self, planned: PlannedFetch) {
        self.queue.push_back(planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    pub(in crate::variant) fn push_front(&mut self, planned: PlannedFetch) {
        self.queue.push_front(planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    /// Promote reader demand without superseding the plan or preceding its init.
    pub(in crate::variant) fn prioritize_segment(&mut self, segment: u32) {
        let planned = PlannedFetch::Segment(segment);
        if let Some(at) = self.queue.iter().position(|queued| *queued == planned) {
            self.queue.remove(at);
        }
        let at = usize::from(self.queue.front() == Some(&PlannedFetch::Init));
        self.queue.insert(at, planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
    }

    pub(in crate::variant) fn replace_with(&mut self, plan: impl Iterator<Item = PlannedFetch>) {
        self.supersede();
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

    /// Re-enter work popped off a still-current plan. The insert keeps plan
    /// order (`Init` first, segments ascending) rather than taking the front:
    /// dispatch bounds read the queue head, and a retired look-ahead entry
    /// parked there would wall off every nearer segment behind it. Work from
    /// a superseded plan is refused, so a seek cannot resurrect an obsolete
    /// prefix, and an entry the current plan already holds is never doubled.
    pub(in crate::variant) fn requeue_if_current(
        &mut self,
        planned: PlannedFetch,
        revision: PlanRevision,
    ) -> bool {
        if PlanRevision(self.revision.load(Ordering::Relaxed)) != revision
            || self.queue.contains(&planned)
        {
            return false;
        }
        let at = self
            .queue
            .iter()
            .position(|queued| *queued > planned)
            .unwrap_or(self.queue.len());
        self.queue.insert(at, planned);
        PlanQueue::mark(self.init_planned, self.segments_planned, planned, true);
        true
    }

    /// Declare a new plan identity without touching the queue. Every rebuild
    /// supersedes — including one that leaves the plan as-is — so a fetch
    /// cancelled by the rearm that triggered it settles into a foreign plan
    /// and stays off it.
    pub(in crate::variant) fn supersede(&mut self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn plan() -> PlanQueue {
        PlanQueue::new(8, 4)
    }

    #[kithara::test(native, flash(false))]
    fn a_replaced_plan_is_visible_without_the_lock() {
        let queue = plan();

        queue
            .lock()
            .replace_with([PlannedFetch::Init, PlannedFetch::Segment(2)].into_iter());

        assert!(queue.planned(PlannedFetch::Init));
        assert!(queue.planned(PlannedFetch::Segment(2)));
        assert!(!queue.planned(PlannedFetch::Segment(1)));
    }

    #[kithara::test(native, flash(false))]
    fn a_popped_fetch_leaves_the_mirror() {
        let queue = plan();
        queue
            .lock()
            .replace_with([PlannedFetch::Segment(1)].into_iter());

        assert_eq!(
            queue.lock().pop_front().map(|(planned, _)| planned),
            Some(PlannedFetch::Segment(1))
        );

        assert!(!queue.planned(PlannedFetch::Segment(1)));
    }

    #[kithara::test(native, flash(false))]
    fn a_requeued_fetch_reenters_the_mirror() {
        let queue = plan();

        queue.lock().push_front(PlannedFetch::Segment(3));

        assert!(queue.planned(PlannedFetch::Segment(3)));
    }

    #[kithara::test(native, flash(false))]
    fn a_segment_outside_the_map_is_never_planned() {
        let queue = plan();

        queue.lock().push_front(PlannedFetch::Segment(40));

        assert!(!queue.planned(PlannedFetch::Segment(40)));
        assert_eq!(
            queue.lock().pop_front().map(|(planned, _)| planned),
            Some(PlannedFetch::Segment(40))
        );
    }

    #[kithara::test(native, flash(false))]
    fn reader_priority_preserves_init_membership_and_plan_identity() {
        let queue = plan();
        queue.lock().replace_with(
            [
                PlannedFetch::Init,
                PlannedFetch::Segment(0),
                PlannedFetch::Segment(1),
                PlannedFetch::Segment(2),
            ]
            .into_iter(),
        );
        let revision = queue.revision();

        queue.lock().prioritize_segment(2);
        queue.lock().prioritize_segment(2);

        assert_eq!(queue.revision(), revision);
        assert_eq!(
            queue.lock().iter().copied().collect::<Vec<_>>(),
            vec![
                PlannedFetch::Init,
                PlannedFetch::Segment(2),
                PlannedFetch::Segment(0),
                PlannedFetch::Segment(1),
            ]
        );
        assert!(queue.planned(PlannedFetch::Init));
        for segment in 0..3 {
            assert!(queue.planned(PlannedFetch::Segment(segment)));
        }
        assert!(!queue.planned(PlannedFetch::Segment(3)));
        queue.lock().pop_front();
        queue.lock().pop_front();
        assert!(!queue.planned(PlannedFetch::Segment(2)));
    }

    #[kithara::test(native, flash(false))]
    fn reader_priority_inserts_missing_demand_once() {
        let queue = plan();
        queue
            .lock()
            .replace_with([PlannedFetch::Segment(0), PlannedFetch::Segment(1)].into_iter());
        let revision = queue.revision();

        queue.lock().prioritize_segment(3);
        queue.lock().prioritize_segment(3);

        assert_eq!(queue.revision(), revision);
        assert_eq!(
            queue.lock().iter().copied().collect::<Vec<_>>(),
            vec![
                PlannedFetch::Segment(3),
                PlannedFetch::Segment(0),
                PlannedFetch::Segment(1),
            ]
        );
        assert!(queue.planned(PlannedFetch::Segment(3)));
        queue.lock().pop_front();
        assert!(!queue.planned(PlannedFetch::Segment(3)));
    }

    #[kithara::test(native, flash(false))]
    fn a_replaced_plan_rejects_a_stale_requeue() {
        let queue = plan();
        queue
            .lock()
            .replace_with([PlannedFetch::Segment(1)].into_iter());
        let (planned, revision) = queue.lock().pop_front().expect("planned fetch");

        queue
            .lock()
            .replace_with([PlannedFetch::Segment(3)].into_iter());

        assert!(!queue.lock().requeue_if_current(planned, revision));
        assert_eq!(
            queue.lock().iter().copied().collect::<Vec<_>>(),
            vec![PlannedFetch::Segment(3)]
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_requeued_fetch_reenters_in_plan_order() {
        let queue = plan();
        queue
            .lock()
            .replace_with([PlannedFetch::Segment(0), PlannedFetch::Segment(3)].into_iter());
        let (near, revision) = queue.lock().pop_front().expect("near fetch");
        let (far, _) = queue.lock().pop_front().expect("far fetch");

        assert!(queue.lock().requeue_if_current(near, revision));
        assert!(queue.lock().requeue_if_current(far, revision));

        assert_eq!(
            queue.lock().iter().copied().collect::<Vec<_>>(),
            vec![PlannedFetch::Segment(0), PlannedFetch::Segment(3)],
            "a returning far entry must not park in front of a nearer one"
        );
    }
}
