use kithara_platform::time::Duration;
use kithara_test_utils::hang::{HangDetector, default_timeout};
use serde::Serialize;
use tracing::{debug, warn};

use crate::runtime::{PassReport, SchedulerEvent, SchedulerObserver};

#[derive(Clone, Copy, Default, Serialize)]
struct AnalysisHangContext {
    last_report: Option<PassReport>,
    waiting_streak: u32,
}

pub(crate) struct AnalysisObserver {
    context: AnalysisHangContext,
    detector: HangDetector<AnalysisHangContext>,
}

impl AnalysisObserver {
    const HEAVY_TICK_BUDGET: Duration = Duration::from_secs(120);

    fn reset_with_report(&mut self, report: PassReport) {
        self.context = AnalysisHangContext {
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

impl Default for AnalysisObserver {
    fn default() -> Self {
        Self {
            context: AnalysisHangContext::default(),
            detector: HangDetector::new("analysis_worker_loop", default_timeout()),
        }
    }
}

impl SchedulerObserver for AnalysisObserver {
    fn on_event(&mut self, event: SchedulerEvent) {
        match event {
            SchedulerEvent::Progress(report)
            | SchedulerEvent::Idle(report)
            | SchedulerEvent::UpstreamPending(report)
            | SchedulerEvent::Backpressured(report) => self.reset_with_report(report),
            SchedulerEvent::Waiting(report) => self.tick_with_report(report),
            SchedulerEvent::SlowTick { slot, elapsed } if elapsed > Self::HEAVY_TICK_BUDGET => {
                warn!(
                    slot_id = slot,
                    elapsed_ms = elapsed.as_millis(),
                    budget_ms = Self::HEAVY_TICK_BUDGET.as_millis(),
                    "analysis heavy tick exceeded hang budget"
                );
            }
            SchedulerEvent::SlowTick { slot, elapsed } => {
                debug!(
                    slot_id = slot,
                    elapsed_ms = elapsed.as_millis(),
                    budget_ms = Self::HEAVY_TICK_BUDGET.as_millis(),
                    "analysis heavy tick completed within its budget"
                );
            }
            SchedulerEvent::PassStart | SchedulerEvent::PassEnd => {}
        }
    }
}
