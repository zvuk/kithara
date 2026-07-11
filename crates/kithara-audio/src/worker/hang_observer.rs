use kithara_test_utils::hang::{HangDetector, default_timeout};
use serde::Serialize;

use crate::runtime::{PassReport, SchedulerEvent, SchedulerObserver};

#[derive(Clone, Copy, Default, Serialize)]
struct AudioWorkerHangContext {
    last_report: Option<PassReport>,
    waiting_streak: u32,
}

/// Observer that integrates the scheduler with `kithara_test_utils::hang`.
pub(crate) struct HangWatchdogObserver {
    context: AudioWorkerHangContext,
    detector: HangDetector<AudioWorkerHangContext>,
}

impl HangWatchdogObserver {
    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self {
            context: AudioWorkerHangContext::default(),
            detector: HangDetector::new("audio_worker_loop", default_timeout()),
        }
    }

    fn reset_with_report(&mut self, report: PassReport) {
        self.context = AudioWorkerHangContext {
            last_report: Some(report),
            waiting_streak: 0,
        };
        let context = self.context;
        self.detector.reset_with(|| context);
    }

    fn tick_with_report(&mut self, report: PassReport) {
        self.context.last_report = Some(report);
        self.context.waiting_streak = self.context.waiting_streak.saturating_add(1);
        let context = self.context;
        self.detector.tick_with(|| context);
    }
}

impl SchedulerObserver for HangWatchdogObserver {
    fn on_event(&mut self, event: SchedulerEvent) {
        match event {
            SchedulerEvent::Progress(report)
            | SchedulerEvent::Idle(report)
            | SchedulerEvent::UpstreamPending(report)
            | SchedulerEvent::Backpressured(report) => self.reset_with_report(report),
            SchedulerEvent::Waiting(report) => {
                self.tick_with_report(report);
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::{hang::HangDump, kithara};

    use super::*;
    use crate::runtime::{PassOutcome, ServiceClass};

    #[kithara::test]
    fn hang_context_json_includes_scheduler_report() {
        let mut report = PassReport::new(2);
        report.record(
            7,
            ServiceClass::Audible,
            crate::runtime::TickResult::Waiting,
        );
        report.record(
            9,
            ServiceClass::Warm,
            crate::runtime::TickResult::UpstreamPending,
        );
        report.outcome = PassOutcome::Waiting;

        let json = AudioWorkerHangContext {
            last_report: Some(report),
            waiting_streak: 3,
        }
        .dump_json();

        assert!(json.contains("\"waiting_streak\":3"));
        assert!(json.contains("\"outcome\":\"waiting\""));
        assert!(json.contains("\"active_slots\":2"));
        assert!(json.contains("\"waiting_slots\":1"));
        assert!(json.contains("\"upstream_pending_slots\":1"));
        assert!(json.contains("\"first_waiting_slot\":7"));
        assert!(json.contains("\"first_waiting_service_class\":\"audible\""));
    }
}
