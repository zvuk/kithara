use kithara_platform::time::Duration;

use crate::runtime::SlotId;

/// Best result from a single round-robin pass over all nodes.
///
/// Ordered by priority: `Produced > Waiting > Backpressured > Idle`.
/// A single node returning `Progress` overrides everything else.
/// `Waiting` (upstream-blocked) outranks `Backpressured`
/// (downstream-not-pulling) so a real source hang is still detected
/// when at least one slot is genuinely stuck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassOutcome {
    /// At least one node made progress.
    Produced,
    /// No progress made and at least one node is upstream-blocked
    /// (source not ready). Ticking the hang watchdog here turns a
    /// forever-stuck source into a panic at the configured budget.
    Waiting,
    /// No progress made and every non-terminal node is downstream-
    /// backpressured (PCM ring full / consumer not pulling). The hang
    /// watchdog must NOT tick — this is the paused/idle player state.
    Backpressured,
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
    /// Every non-terminal slot is downstream-backpressured (its
    /// downstream consumer is not pulling, so the PCM ring is full
    /// and the node cannot push more). Progress is not expected until
    /// the consumer drains the ring, so this state must NOT tick the
    /// hang watchdog — an idle/paused player is the typical cause.
    Backpressured,
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
