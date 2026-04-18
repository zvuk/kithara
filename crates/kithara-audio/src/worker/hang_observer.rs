//! Watchdog observer for the audio worker.

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
            SchedulerEvent::Progress | SchedulerEvent::Idle => {
                self.detector.reset();
            }
            SchedulerEvent::SlowTick { slot, elapsed } => {
                tracing::warn!(
                    track_id = slot,
                    elapsed_ms = elapsed.as_millis(),
                    "step_track took too long — starving other tracks"
                );
            }
            SchedulerEvent::PassStart | SchedulerEvent::PassEnd => {}
        }
    }
}
