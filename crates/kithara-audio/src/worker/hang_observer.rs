use kithara_hang_detector::{HangDetector, default_timeout};

use crate::runtime::{SchedulerEvent, SchedulerObserver};

/// Observer that integrates the scheduler with `kithara_hang_detector`.
pub(crate) struct HangWatchdogObserver {
    detector: HangDetector,
}

impl HangWatchdogObserver {
    pub(crate) fn new() -> Self {
        Self {
            detector: HangDetector::new("audio_worker_loop", default_timeout()),
        }
    }
}

impl SchedulerObserver for HangWatchdogObserver {
    fn on_event(&mut self, event: SchedulerEvent) {
        match event {
            // Real audio progress (PCM chunk produced) clears the deadline.
            // `Idle` also clears it — no slots at all means no work expected
            // (paused / nothing loaded). `Backpressured` clears it as well:
            // every non-terminal slot is parked because its downstream
            // consumer is not pulling, so progress is *not* expected until
            // the consumer drains — ticking the watchdog here would
            // false-positive an idle/paused player as a hang.
            SchedulerEvent::Progress | SchedulerEvent::Idle | SchedulerEvent::Backpressured => {
                self.detector.reset();
            }
            // A slot exists, the downstream is pulling, but the source
            // produced nothing this pass — the audio worker is parked on
            // a blocked source. Tick the watchdog so a never-ready source
            // surfaces as a panic at the configured budget instead of an
            // indefinite park.
            SchedulerEvent::Waiting => {
                self.detector.tick();
            }
            SchedulerEvent::SlowTick { slot, elapsed } => {
                tracing::debug!(
                    track_id = slot,
                    elapsed_ms = elapsed.as_millis(),
                    "step_track took too long — starving other tracks"
                );
            }
            SchedulerEvent::PassStart | SchedulerEvent::PassEnd => {}
        }
    }
}
