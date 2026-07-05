use std::time::Duration;

use cpu_time::ThreadTime;

pub(super) fn thread_cpu_now() -> Option<ThreadTime> {
    ThreadTime::try_now().ok()
}

pub(super) fn thread_cpu_elapsed(start: Option<ThreadTime>) -> Option<Duration> {
    start.and_then(|s| s.try_elapsed().ok())
}
