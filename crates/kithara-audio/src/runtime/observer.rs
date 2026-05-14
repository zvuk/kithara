use kithara_platform::time::Duration;

use crate::runtime::SlotId;

/// Best result from a single round-robin pass over all nodes.
///
/// Ordered by priority: `Produced > Waiting > Idle`.
/// A single node returning `Progress` overrides everything else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassOutcome {
    /// At least one node made progress.
    Produced,
    /// No progress made, but at least one node is waiting (e.g. backpressure).
    Waiting,
    /// All nodes are terminal (Done) — legitimately idle.
    Idle,
}

/// Events emitted by the scheduler during its loop.
pub(crate) enum SchedulerEvent {
    /// A new pass over the nodes is starting.
    PassStart,
    /// A pass over the nodes has finished.
    PassEnd,
    /// At least one node made progress in the last pass.
    Progress,
    /// All nodes are terminal.
    Idle,
    /// At least one node has a non-terminal slot but produced no progress
    /// this pass (slot blocked on the source). Distinct from `Idle`
    /// (which means "no work expected"): `Waiting` is the cooperative-yield
    /// state where the audio worker *should* be making progress but is
    /// parked. The `HangWatchdogObserver` ticks the watchdog here so a
    /// blocked-forever source surfaces as a hang panic instead of an
    /// indefinite park.
    Waiting,
    /// A node took too long to tick.
    SlowTick {
        /// The ID of the slow node.
        slot: SlotId,
        /// How long the tick took.
        elapsed: Duration,
    },
}

/// A plugin that observes the scheduler's execution.
pub(crate) trait SchedulerObserver: Send + 'static {
    /// Called when a scheduler event occurs.
    fn on_event(&mut self, event: SchedulerEvent);
}
