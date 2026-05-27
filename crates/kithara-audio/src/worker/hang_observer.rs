use kithara_test_utils::hang::{HangDetector, default_timeout};

use crate::runtime::{SchedulerEvent, SchedulerObserver};

/// Observer that integrates the scheduler with `kithara_test_utils::hang`.
pub(crate) struct HangWatchdogObserver {
    detector: HangDetector,
}

impl HangWatchdogObserver {
    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self {
            detector: HangDetector::new("audio_worker_loop", default_timeout()),
        }
    }
}

impl SchedulerObserver for HangWatchdogObserver {
    fn on_event(&mut self, event: SchedulerEvent) {
        match event {
            SchedulerEvent::Progress | SchedulerEvent::Idle | SchedulerEvent::Backpressured => {
                self.detector.reset();
            }
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
