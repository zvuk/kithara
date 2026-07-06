#[cfg(test)]
use std::cell::Cell;
use std::time::Duration;

use cpu_time::ThreadTime;

#[cfg(test)]
thread_local! {
    static FORCED_CPU_ELAPSED: Cell<Option<Duration>> = const { Cell::new(None) };
}

pub(super) fn thread_cpu_now() -> Option<ThreadTime> {
    ThreadTime::try_now().ok()
}

pub(super) fn thread_cpu_elapsed(start: Option<ThreadTime>) -> Option<Duration> {
    #[cfg(test)]
    {
        if let Some(cpu) = FORCED_CPU_ELAPSED.with(Cell::get) {
            return Some(cpu);
        }
    }

    start.and_then(|s| s.try_elapsed().ok())
}

#[cfg(test)]
pub(crate) fn force_cpu_elapsed(cpu: Option<Duration>) {
    FORCED_CPU_ELAPSED.with(|forced| forced.set(cpu));
}
