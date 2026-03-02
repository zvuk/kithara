use std::time::{Duration, Instant};

const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(1);

pub struct CrossfadeClock {
    duration: Duration,
    started_at: Instant,
}

impl CrossfadeClock {
    #[must_use]
    pub fn new(duration_secs: f32) -> Self {
        Self {
            duration: Duration::from_secs_f32(duration_secs.max(0.0)),
            started_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn progress(&self) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let progress = self.started_at.elapsed().as_secs_f32() / self.duration.as_secs_f32();
        progress.clamp(0.0, 1.0)
    }
}

pub struct ProgressLog {
    last_emit: Instant,
}

impl Default for ProgressLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_emit: Instant::now() - PROGRESS_LOG_INTERVAL,
        }
    }

    pub fn should_emit(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_emit) < PROGRESS_LOG_INTERVAL {
            return false;
        }
        self.last_emit = now;
        true
    }
}
