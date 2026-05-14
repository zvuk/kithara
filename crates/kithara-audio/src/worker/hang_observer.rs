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
            // (paused / nothing loaded); ticking here would false-positive
            // an idle player as a hang.
            SchedulerEvent::Progress | SchedulerEvent::Idle => {
                self.detector.reset();
            }
            // A slot exists but produced nothing this pass — the audio
            // worker is parked on a blocked source. Tick the watchdog so
            // a never-ready source surfaces as a panic at the default
            // budget instead of an indefinite park.
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
