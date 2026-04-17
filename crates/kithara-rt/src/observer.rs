//! Observer plugin for the scheduler.

use std::time::Duration;

use crate::scheduler::SlotId;

/// Best result from a single round-robin pass over all nodes.
///
/// Ordered by priority: `Produced > Waiting > Idle > Stuck`.
/// A single node returning `Progress` overrides everything else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassOutcome {
    /// At least one node made progress.
    Produced,
    /// No progress made, but at least one node is waiting (e.g. backpressure).
    Waiting,
    /// All nodes are terminal (Done) — legitimately idle.
    Idle,
    /// Non-terminal nodes reported no progress — potential hang.
    Stuck,
}

/// Events emitted by the scheduler during its loop.
pub enum SchedulerEvent {
    /// A new pass over the nodes is starting.
    PassStart,
    /// A pass over the nodes has finished.
    PassEnd(PassOutcome),
    /// At least one node made progress in the last pass.
    Progress,
    /// No nodes made progress, but some are not terminal (potential hang).
    Stuck,
    /// All nodes are terminal.
    Idle,
    /// A node took too long to tick.
    SlowTick {
        /// The ID of the slow node.
        slot: SlotId,
        /// How long the tick took.
        elapsed: Duration,
    },
}

/// A plugin that observes the scheduler's execution.
pub trait SchedulerObserver: Send + 'static {
    /// Called when a scheduler event occurs.
    fn on_event(&mut self, event: SchedulerEvent);
}

/// A default observer that does nothing.
pub struct NoopObserver;

impl SchedulerObserver for NoopObserver {
    fn on_event(&mut self, _event: SchedulerEvent) {}
}
