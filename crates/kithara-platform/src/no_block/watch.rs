use std::{
    future::Future,
    panic::Location,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use pin_project_lite::pin_project;

use super::{
    clock, ctx,
    ctx::{Permit, PollScope},
    mode, report,
};

#[derive(Clone, Copy)]
pub(super) enum Tier {
    Blanket,
    Strict,
}

pin_project! {
    pub struct PermitPoll<F> {
        #[pin]
        fut: F,
    }
}

pin_project! {
    pub struct Watched<F> {
        #[pin]
        fut: F,
        name: &'static str,
        loc: &'static Location<'static>,
        budget: Duration,
        tier: Tier,
    }
}

impl<F: Future> Future for PermitPoll<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _permit = Permit::enter();
        this.fut.poll(cx)
    }
}

impl<F: Future> Future for Watched<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if mode::is_off() {
            return this.fut.poll(cx);
        }

        let paused_before = ctx::paused_nanos();
        let paused_cpu_before = ctx::paused_cpu_nanos();
        let wall_start = Instant::now();
        // WHY: Snapshot can be up to 1 ms old, which is safe: budgets are 25 ms strict / 3000 ms blanket.
        let cpu_start = clock::snapshot(wall_start);
        let res = {
            let _scope = PollScope::enter((this.name, this.loc));
            this.fut.poll(cx)
        };
        let paused = nanos_since(ctx::paused_nanos(), paused_before);
        let wall = wall_start.elapsed().saturating_sub(paused);
        if wall > *this.budget {
            let paused_cpu = nanos_since(ctx::paused_cpu_nanos(), paused_cpu_before);
            let cpu =
                clock::thread_cpu_elapsed(cpu_start).map(|cpu| cpu.saturating_sub(paused_cpu));
            report::over_budget(this.name, this.loc, wall, cpu, *this.budget, *this.tier);
        }
        res
    }
}

fn nanos_since(now: u128, before: u128) -> Duration {
    Duration::from_nanos(u64::try_from(now.saturating_sub(before)).unwrap_or(u64::MAX))
}

#[doc(hidden)]
#[must_use]
pub fn permit_poll<F: Future>(fut: F) -> PermitPoll<F> {
    PermitPoll { fut }
}

#[must_use]
#[track_caller]
pub fn watch_blanket<F: Future>(name: &'static str, fut: F) -> Watched<F> {
    watch_blanket_at(name, Location::caller(), fut)
}

#[doc(hidden)]
#[must_use]
pub fn watch_blanket_at<F: Future>(
    name: &'static str,
    loc: &'static Location<'static>,
    fut: F,
) -> Watched<F> {
    Watched {
        fut,
        name,
        loc,
        budget: mode::blanket_budget(),
        tier: Tier::Blanket,
    }
}

#[doc(hidden)]
#[must_use]
#[track_caller]
pub fn watch_budget<F: Future>(name: &'static str, budget_ms: u64, fut: F) -> Watched<F> {
    Watched {
        fut,
        name,
        loc: Location::caller(),
        budget: Duration::from_millis(budget_ms),
        tier: Tier::Strict,
    }
}
