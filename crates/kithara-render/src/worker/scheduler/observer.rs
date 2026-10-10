use kithara_test_utils::hang::{HangDetector, default_timeout};
use kithara_worker::{Event, Observer, PassOutcome, PassReport, TaskId};
use serde::Serialize;

#[derive(Clone, Copy, Default, Serialize)]
struct PlaybackHangContext {
    first_waiting_task: Option<u64>,
    active_tasks: usize,
    waiting_tasks: usize,
}

impl From<PassReport> for PlaybackHangContext {
    fn from(report: PassReport) -> Self {
        Self {
            active_tasks: report.active_tasks,
            first_waiting_task: report.first_waiting_task.map(TaskId::get),
            waiting_tasks: report.waiting_tasks,
        }
    }
}

pub(crate) struct PlaybackObserver {
    detector: HangDetector<PlaybackHangContext>,
}

impl PlaybackObserver {
    fn observe(&mut self, report: PassReport) {
        let context = PlaybackHangContext::from(report);
        if report.outcome == PassOutcome::Waiting {
            self.detector.tick_with(|| context);
        } else {
            self.detector.reset_with(|| context);
        }
        tracing::trace!(
            ?report.outcome,
            active_tasks = report.active_tasks,
            progress_tasks = report.progress_tasks,
            waiting_tasks = report.waiting_tasks,
            upstream_pending_tasks = report.upstream_pending_tasks,
            backpressured_tasks = report.backpressured_tasks,
            done_tasks = report.done_tasks,
            "playback scheduler pass"
        );
    }
}

impl Default for PlaybackObserver {
    fn default() -> Self {
        Self {
            detector: HangDetector::new("audio_worker_loop", default_timeout()),
        }
    }
}

impl Observer for PlaybackObserver {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::SlowTick { task, elapsed } => {
                tracing::debug!(
                    track_id = task.get(),
                    elapsed_ms = elapsed.as_millis(),
                    "step_track took too long - starving other tracks"
                );
            }
            Event::TaskPanicked { task } => {
                tracing::warn!(track_id = task.get(), "playback task panicked");
            }
            Event::ThreadClassRefused { class, error } => {
                tracing::warn!(
                    ?class,
                    ?error,
                    "the OS refused the playback worker's thread class; \
                     a busy machine can starve the decks' rings"
                );
            }
            Event::Progress(report)
            | Event::Idle(report)
            | Event::Waiting(report)
            | Event::UpstreamPending(report)
            | Event::Backpressured(report) => self.observe(report),
            _ => {}
        }
    }
}
