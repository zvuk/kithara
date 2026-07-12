use std::{
    future::Future,
    panic::Location,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use pin_project_lite::pin_project;

use super::{clock, ctx, mode, report};

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
        let _permit = ctx::Permit::enter();
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
        let wall_start = Instant::now();
        // Snapshot can be up to 1 ms old, which is safe: budgets are 25 ms strict / 3000 ms blanket.
        let cpu_start = clock::snapshot(wall_start);
        let res = {
            let _scope = ctx::PollScope::enter((this.name, this.loc));
            this.fut.poll(cx)
        };
        let paused = ctx::paused_nanos().saturating_sub(paused_before);
        let paused = Duration::from_nanos(u64::try_from(paused).unwrap_or(u64::MAX));
        let wall = wall_start.elapsed().saturating_sub(paused);
        if wall > *this.budget {
            let cpu = clock::thread_cpu_elapsed(cpu_start);
            report::over_budget(this.name, this.loc, wall, cpu, *this.budget, *this.tier);
        }
        res
    }
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
