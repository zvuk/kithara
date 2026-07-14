use std::sync::atomic::{AtomicU8, Ordering};

use serde::Serialize;

/// Priority class for worker scheduling.
///
/// Nodes with higher service class are served first when the scheduler
/// selects which node to process next.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceClass {
    /// Not playing, not needed soon. Lowest priority.
    #[default]
    Idle,
    /// Preloading or about to play. Medium priority.
    Warm,
    /// Currently audible. Highest priority.
    Audible,
}

impl From<ServiceClass> for u8 {
    fn from(class: ServiceClass) -> Self {
        match class {
            ServiceClass::Idle => 0,
            ServiceClass::Warm => 1,
            ServiceClass::Audible => 2,
        }
    }
}

impl From<u8> for ServiceClass {
    fn from(value: u8) -> Self {
        match value {
            2 => Self::Audible,
            1 => Self::Warm,
            _ => Self::Idle,
        }
    }
}

/// Lock-free shared `ServiceClass`, written wait-free by the real-time
/// consumer (`Audio::set_service_class` during fade transitions) and read
/// by the worker scheduler each pass. Avoids the scheduler command channel
/// — and its periodic allocation — on the real-time audio thread.
pub(crate) struct AtomicServiceClass(AtomicU8);

impl AtomicServiceClass {
    pub(crate) fn new(class: ServiceClass) -> Self {
        Self(AtomicU8::new(class.into()))
    }

    pub(crate) fn load(&self) -> ServiceClass {
        self.0.load(Ordering::Relaxed).into()
    }

    pub(crate) fn store(&self, class: ServiceClass) {
        self.0.store(class.into(), Ordering::Relaxed);
    }
}

/// Result of a single node tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TickResult {
    /// Node made progress (produced or consumed data, applied internal state change).
    Progress,
    /// Node is alive but waiting on an upstream source (`step_track()`
    /// returned `Blocked`). The scheduler interprets this as
    /// "progress is *expected* but not happening" — the hang watchdog
    /// ticks here so a forever-blocked source surfaces as a panic
    /// instead of an indefinite park.
    Waiting,
    /// Node is alive but its downstream consumer is not pulling
    /// (PCM ring full / outlet overflow). The scheduler treats this
    /// as a paused/idle player — progress is *not expected* until the
    /// consumer drains the ring, so the hang watchdog must NOT tick.
    /// Distinguishing this from `Waiting` is what keeps an idle
    /// `Audio` handle from panicking after the watchdog budget
    /// expires (the symptom that prompted bug #1).
    Backpressured,
    /// Node is alive and the upstream source has an active demand/fetch
    /// for the blocked range. The scheduler parks briefly and relies on
    /// the source/downloader's own terminal timeout/cancel contract
    /// instead of ticking the audio-worker hang detector as if no demand
    /// existed.
    UpstreamPending,
    /// Node has finished its work (EOF, failed, terminal).
    Done,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RtPolicy {
    /// Tick under the realtime sanitizer's forbid-blocking scope.
    #[default]
    Rt,
    /// Tick outside the forbid-blocking scope.
    Heavy,
}

/// A component that can be executed by the scheduler.
pub(crate) trait Node: Send + 'static {
    /// Called when the scheduler is cancelled or the node is unregistered.
    fn on_cancel(&mut self) {}

    /// Reclaim deferred bookkeeping (free/recycle spent buffers) outside the
    /// forbid-blocking produce core. Run once per pass by the scheduler shell
    /// before [`tick`](Node::tick), so a `free` on a full pool never lands on
    /// the checked produce path. Default no-op.
    fn recycle(&mut self) {}

    /// Scheduling policy, cached when the node is registered.
    fn rt_policy(&self) -> RtPolicy {
        RtPolicy::Rt
    }

    /// Return the current service class (priority) of this node.
    fn service_class(&self) -> ServiceClass {
        ServiceClass::Audible
    }

    /// Perform one quantum of work.
    fn tick(&mut self) -> TickResult;

    /// One-time worker-thread warmup, run by the scheduler shell when the node
    /// is registered — before any [`tick`](Node::tick) reaches the
    /// forbid-blocking produce core. Pre-touches lazy global thread-locals the
    /// produce-core read path would otherwise allocate on first use (notably
    /// `arc_swap`'s per-thread debt node, hit via the storage committed-read
    /// snapshot). Default no-op.
    fn warm_up(&mut self) {}
}

impl Node for Box<dyn Node> {
    fn on_cancel(&mut self) {
        (**self).on_cancel();
    }

    fn recycle(&mut self) {
        (**self).recycle();
    }

    fn rt_policy(&self) -> RtPolicy {
        (**self).rt_policy()
    }

    fn service_class(&self) -> ServiceClass {
        (**self).service_class()
    }

    fn tick(&mut self) -> TickResult {
        (**self).tick()
    }

    fn warm_up(&mut self) {
        (**self).warm_up();
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn service_class_ordering() {
        assert!(ServiceClass::Idle < ServiceClass::Warm);
        assert!(ServiceClass::Warm < ServiceClass::Audible);
    }

    #[kithara::test]
    fn service_class_default_is_idle() {
        assert_eq!(ServiceClass::default(), ServiceClass::Idle);
    }

    struct DefaultPolicyNode;

    impl Node for DefaultPolicyNode {
        fn tick(&mut self) -> TickResult {
            TickResult::Done
        }
    }

    #[kithara::test]
    fn node_rt_policy_defaults_to_rt() {
        assert_eq!(DefaultPolicyNode.rt_policy(), RtPolicy::Rt);
    }
}
