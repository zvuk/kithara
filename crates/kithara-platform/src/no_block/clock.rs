use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use cpu_time::ThreadTime;

thread_local! {
    static SNAPSHOT: Cell<Option<(ThreadTime, Instant)>> = const { Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    static SNAPSHOT_REFRESH_COUNT: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    static FORCED_CPU_ELAPSED: Cell<Option<Duration>> = const { Cell::new(None) };
}

const SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(1);

pub(super) fn snapshot(now: Instant) -> Option<ThreadTime> {
    SNAPSHOT.with(|snapshot| {
        if let Some((cpu, sampled_at)) = snapshot.get()
            && now
                .checked_duration_since(sampled_at)
                .is_some_and(|age| age < SNAPSHOT_MAX_AGE)
        {
            return Some(cpu);
        }

        if let Ok(cpu) = ThreadTime::try_now() {
            #[cfg(test)]
            SNAPSHOT_REFRESH_COUNT.with(|count| count.set(count.get() + 1));
            snapshot.set(Some((cpu, now)));
            return Some(cpu);
        }

        snapshot.set(None);
        None
    })
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

#[cfg(test)]
pub(crate) fn snapshot_refresh_count() -> u32 {
    SNAPSHOT_REFRESH_COUNT.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn force_snapshot_refresh_count(refresh_count: u32) {
    SNAPSHOT.with(|snapshot| snapshot.set(None));
    SNAPSHOT_REFRESH_COUNT.with(|count| count.set(refresh_count));
}
