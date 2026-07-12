use kithara_platform::time::Duration;
use serde::Serialize;

use crate::runtime::{ServiceClass, SlotId, TickResult};

/// Best result from a single round-robin pass over all nodes.
///
/// Ordered by priority:
/// `Produced > Waiting > UpstreamPending > Backpressured > Idle`.
/// A single node returning `Progress` overrides everything else.
/// `Waiting` (upstream-blocked) outranks `Backpressured`
/// (downstream-not-pulling) so a real source hang is still detected
/// when at least one slot is genuinely stuck.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PassOutcome {
    /// At least one node made progress.
    Produced,
    /// No progress made and at least one node is upstream-blocked
    /// (source not ready). Ticking the hang watchdog here turns a
    /// forever-stuck source into a panic at the configured budget.
    Waiting,
    /// No progress made and at least one node is waiting on an already
    /// in-flight upstream demand. This is not a lost wake/dispatch; the
    /// source layer owns terminal failure detection for the fetch.
    UpstreamPending,
    /// No progress made and every non-terminal node is downstream-
    /// backpressured (PCM ring full / consumer not pulling). The hang
    /// watchdog must NOT tick — this is the paused/idle player state.
    Backpressured,
    /// All nodes are terminal (Done) — legitimately idle.
    Idle,
}

/// Allocation-free summary of one scheduler pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct PassReport {
    pub(crate) outcome: PassOutcome,
    pub(crate) active_slots: usize,
    pub(crate) progress_slots: usize,
    pub(crate) waiting_slots: usize,
    pub(crate) upstream_pending_slots: usize,
    pub(crate) backpressured_slots: usize,
    pub(crate) done_slots: usize,
    pub(crate) first_progress_slot: Option<SlotId>,
    pub(crate) first_waiting_slot: Option<SlotId>,
    pub(crate) first_upstream_pending_slot: Option<SlotId>,
    pub(crate) first_backpressured_slot: Option<SlotId>,
    pub(crate) first_waiting_service_class: Option<ServiceClass>,
}

impl PassReport {
    pub(crate) fn new(active_slots: usize) -> Self {
        Self {
            active_slots,
            outcome: PassOutcome::Idle,
            progress_slots: 0,
            waiting_slots: 0,
            upstream_pending_slots: 0,
            backpressured_slots: 0,
            done_slots: 0,
            first_progress_slot: None,
            first_waiting_slot: None,
            first_upstream_pending_slot: None,
            first_backpressured_slot: None,
            first_waiting_service_class: None,
        }
    }

    pub(crate) fn record(&mut self, slot: SlotId, service_class: ServiceClass, result: TickResult) {
        match result {
            TickResult::Progress => {
                self.progress_slots += 1;
                self.first_progress_slot.get_or_insert(slot);
            }
            TickResult::Waiting => {
                self.waiting_slots += 1;
                self.first_waiting_slot.get_or_insert(slot);
                self.first_waiting_service_class
                    .get_or_insert(service_class);
            }
            TickResult::UpstreamPending => {
                self.upstream_pending_slots += 1;
                self.first_upstream_pending_slot.get_or_insert(slot);
            }
            TickResult::Backpressured => {
                self.backpressured_slots += 1;
                self.first_backpressured_slot.get_or_insert(slot);
            }
            TickResult::Done => {
                self.done_slots += 1;
            }
        }
    }
}

/// Events emitted by the scheduler during its loop.
pub(crate) enum SchedulerEvent {
    /// A new pass over the nodes is starting.
    PassStart,
    /// A pass over the nodes has finished.
    PassEnd,
    /// At least one node made progress in the last pass.
    Progress(PassReport),
    /// All nodes are terminal.
    Idle(PassReport),
    /// At least one node has a non-terminal slot but produced no progress
    /// this pass (slot blocked on the source). Distinct from `Idle`
    /// (which means "no work expected"): `Waiting` is the cooperative-yield
    /// state where the audio worker *should* be making progress but is
    /// parked. The `HangWatchdogObserver` ticks the watchdog here so a
    /// blocked-forever source surfaces as a hang panic instead of an
    /// indefinite park.
    Waiting(PassReport),
    /// Source demand is already in flight; park briefly without ticking
    /// the audio-worker hang detector.
    UpstreamPending(PassReport),
    /// Every non-terminal slot is downstream-backpressured (its
    /// downstream consumer is not pulling, so the PCM ring is full
    /// and the node cannot push more). Progress is not expected until
    /// the consumer drains the ring, so this state must NOT tick the
    /// hang watchdog — an idle/paused player is the typical cause.
    Backpressured(PassReport),
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
