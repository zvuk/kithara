use std::{
    backtrace::Backtrace,
    panic::Location,
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
    time::Duration as StdDuration,
};

use super::{
    Clock, Core, CvDesc, CvId, FlashInner, Registry, WaiterId,
    credit::{self, WaitGuard},
    gate::TaskGate,
    state::{AtomicTaskState, ParkOutcome, TaskDiag, TaskState, WakeOutcome},
    wake::{Token, Wake},
};
use crate::{
    common::time::Instant as RealInstant,
    flash::{ctx, diag, ids::ThreadKey},
    sync::Arc,
};

/// What kind of waiter an [`Entry`] is, so a signal targets the right group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaitKind {
    /// A timed waiter with no early-wake channel: woken solely by the engine
    /// crossing its deadline. Used by the test harness `park_for` and by the
    /// async `sleep` future (`register_sleep_async`).
    Timed,
    /// An unparkable thread park (`park_timeout`): woken by the deadline OR by
    /// [`FlashInner::unpark`] targeting this thread id.
    Thread(ThreadKey),
    /// A condvar waiter: woken by the deadline (when timed) OR by
    /// [`FlashInner::signal_condvar`] for this condvar id.
    Condvar(CvId),
}

/// Where a deadline-less waiter parked, for the hang dump. Only `indef` entries
/// carry it: a timed waiter is released by the engine crossing its deadline, so
/// its parking site names nothing the deadline does not already name.
pub(super) struct Parked {
    /// The stack that parked, only under `KITHARA_FLASH_SYNC_BT`. Unlike the
    /// dump's own backtrace this belongs to the waiter, not to the watchdog that
    /// took the snapshot, so it names the frame that owes the wait.
    pub(super) stack: Option<Backtrace>,
    /// The OS thread that parked. Read for SYNC waiters — an async waiter's
    /// parking thread is merely whichever worker polled it. Against
    /// [`Registry::bridged`] it says whether the wait is held from INSIDE an
    /// async poll, i.e. whether it strands the tasks that thread owes a poll.
    pub(super) thread: ThreadKey,
}

impl Parked {
    /// Capture the current parking site. Callers MUST call this BEFORE taking
    /// the `core` lock: a stack walk under `core` would hold the whole engine
    /// for its duration.
    fn here() -> Self {
        Self {
            thread: credit::current_thread_key(),
            stack: diag::parked_backtrace(),
        }
    }
}

/// How far, in nanoseconds, the paced clock may trail real time when a
/// deadline is registered: a few OS timer quanta.
pub(super) const MAX_PACE_LAG_NANOS: u64 = 50_000_000;

/// What a timed park's deadline MEANS to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::flash) enum ParkRole {
    /// The deadline is the event: nothing else releases this waiter on time,
    /// so the clock owes it a stop.
    Deadline,
    /// The deadline is a BACKSTOP under a [`WaitGate`](crate::sync::WaitGate)
    /// edge: the waiter expects a signal and carries a timeout only so a
    /// missed edge cannot wedge it. Re-checking the edge when nothing has
    /// happened observes nothing, so the clock bounds such a park instead of
    /// stopping at it.
    Backstop,
}

/// A parked waiter's wake handle plus the group it belongs to.
pub(super) struct Entry {
    /// `Some` for deadline-less waiters only. See [`Parked`].
    pub(super) parked: Option<Parked>,
    /// What this entry's deadline means. A [`ParkRole::Backstop`] entry never
    /// becomes the advance target while some other deadline exists; a jump that
    /// passes it still wakes it.
    pub(super) role: ParkRole,
    pub(super) kind: WaitKind,
    pub(super) wake: Wake,
}

impl Entry {
    /// A waiter whose deadline is only a backstop under an edge. See
    /// [`ParkRole::Backstop`].
    fn backstop(kind: WaitKind, wake: Wake) -> Self {
        Self {
            kind,
            wake,
            role: ParkRole::Backstop,
            parked: None,
        }
    }

    /// A waiter with NO deadline: only a matching signal frees it, so when one
    /// of these pins quiescence its parking site IS the diagnosis.
    fn indef(kind: WaitKind, wake: Wake, parked: Parked) -> Self {
        Self {
            kind,
            wake,
            role: ParkRole::Deadline,
            parked: Some(parked),
        }
    }

    /// A waiter the engine itself will release by crossing its deadline, so no
    /// parking site is recorded.
    fn timed(kind: WaitKind, wake: Wake) -> Self {
        Self {
            kind,
            wake,
            role: ParkRole::Deadline,
            parked: None,
        }
    }
}

impl Registry {
    /// Firer-side accounting for a batch of woken waiters, under the `core`
    /// lock: bump `active` ONLY for SYNC (OS-thread) wakes — the bump covers
    /// their real OS wake latency, and a woken thread does NOT re-increment on
    /// return; async (Task) wakes are counted in `active_async` by the spawn
    /// poll-wrapper when next polled, not by the firer. Every wake is marked
    /// granted under the lock so a racing cancel stays consistent.
    fn account_woken(&mut self, woken: &[Wake]) {
        self.active += woken.iter().filter(|w| !w.is_task()).count();
        for w in woken {
            w.mark_granted_under_lock();
        }
    }

    fn fresh_cv(&mut self) -> CvId {
        let id = self.next_cv;
        self.next_cv += 1;
        CvId(id)
    }

    fn fresh_id(&mut self) -> WaiterId {
        let id = self.next_id;
        self.next_id += 1;
        WaiterId(id)
    }

    /// Async slots that genuinely pin quiescence. A counted task normally does:
    /// its poll is queued and the clock must not jump past it. The exception is
    /// a task STRANDED behind a bridged wait — `Runnable` on a runtime with one
    /// poller, that poller blocked on the engine mid-poll. Nothing can move
    /// such a task, so counting it deadlocks the engine: the blocking wait's
    /// deadline is only reached by an advance the task forbids, and the poll
    /// that would clear it only runs once that wait returns. Counted by
    /// subtraction from `active_async`, so a holder map that ever fell behind
    /// the counter errs toward pinning.
    fn pinning_async(&self) -> usize {
        if self.active_async == 0 || self.bridged.is_empty() {
            return self.active_async;
        }
        let stranded = self
            .active_async_holders
            .keys()
            .filter(|id| {
                self.task_diag
                    .get(id)
                    .is_some_and(|diag| diag.stranded_behind(&self.bridged))
            })
            .count();
        self.active_async.saturating_sub(stranded)
    }
}

/// Result of an advance attempt: the wakes to fire after releasing the `core`
/// lock via [`WakeBatch::fire`]. `#[must_use]`: a dropped batch is a lost wakeup.
#[must_use]
pub(crate) struct WakeBatch(Vec<Wake>);

impl WakeBatch {
    /// Fire the wakes after the `core` lock is released. Firing is immediate:
    /// the advance chain runs at full virtual speed.
    pub(crate) fn fire(self) {
        for w in self.0 {
            w.fire();
        }
    }
}

impl Core {
    /// Bounds how far the paced clock may trail real time when a deadline is
    /// registered. A timer's overshoot carries to the next deadline, which is
    /// what keeps short timers at real pace where the OS sleeps in coarse
    /// quanta; real time banked beyond that — an op in flight long before the
    /// deadline existed — would let the deadline fire that much early.
    fn bound_pace_lag(&mut self, clock: &Clock) {
        let Some((real, virt)) = self.sched.pace_anchor else {
            return;
        };
        let now = clock.now_nanos();
        let elapsed = u64::try_from(real.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if elapsed.saturating_sub(now.saturating_sub(virt)) <= MAX_PACE_LAG_NANOS {
            return;
        }
        if let Some(real) =
            RealInstant::now().checked_sub(StdDuration::from_nanos(MAX_PACE_LAG_NANOS))
        {
            self.sched.pace_anchor = Some((real, now));
        }
    }

    /// Real nanoseconds the deadline `min` still owes while an op is in flight.
    fn pace_owed(&self, min: u64) -> u64 {
        self.sched.pace_anchor.map_or(0, |(real, virt)| {
            let elapsed = u64::try_from(real.elapsed().as_nanos()).unwrap_or(u64::MAX);
            min.saturating_sub(virt).saturating_sub(elapsed)
        })
    }

    /// No `pinning_async` veto: a paced target exists only while a real I/O op is in flight, where
    /// the pin must not suppress it (see `try_advance`).
    pub(super) fn pace_target(&self, _clock: &Clock) -> Option<StdDuration> {
        if self.registry.active != 0 {
            return None;
        }
        if self.sched.real_io == 0 {
            return None;
        }
        self.sched.pace_anchor?;
        let (&(min, _), _) = self.sched.timed.iter().next()?;
        Some(StdDuration::from_nanos(self.pace_owed(min)))
    }

    /// Decrement the async slot count under a held `core` lock and return the
    /// advance the quiescent edge may unblock. The caller fires it after
    /// releasing the lock.
    fn release_async(&mut self, clock: &Clock) -> WakeBatch {
        debug_assert!(
            self.registry.active_async > 0,
            "async release without a matching acquire"
        );
        self.registry.active_async -= 1;
        self.try_advance(clock)
    }

    /// Evaluate the advance rule while holding the `core` lock. Returns the
    /// [`WakeBatch`] whose wakes the caller fires AFTER releasing the lock
    /// (firing under the lock would make the woken thread immediately contend
    /// on the engine lock) via [`WakeBatch::fire`]. Operates only on `timed` —
    /// it never fires `indef`, which has no deadline. Fires nothing unless
    /// every participant is parked (`active == 0`) and at least one timed
    /// waiter exists.
    ///
    /// Quiescence requires both the sync and async participant counts at zero, except async slots
    /// no thread can currently poll (`pinning_async`), which do not block the advance while a paced
    /// op is in flight.
    pub(super) fn try_advance(&mut self, clock: &Clock) -> WakeBatch {
        let paced = self.sched.real_io != 0 && self.sched.pace_anchor.is_some();
        if self.registry.active != 0 || (!paced && self.registry.pinning_async() != 0) {
            self.sched.advance_counts.blocked += 1;
            return WakeBatch(Vec::new());
        }
        if !self.sched.yielders.is_empty()
            && self
                .sched
                .timed
                .values()
                .all(|e| matches!(e.kind, WaitKind::Thread(_)))
        {
            let woken: Vec<Wake> = std::mem::take(&mut self.sched.yielders)
                .into_values()
                .collect();
            self.sched.advance_counts.yield_releases += 1;
            self.registry.account_woken(&woken);
            return WakeBatch(woken);
        }
        let Some((&(earliest, _), _)) = self.sched.timed.iter().next() else {
            self.sched.advance_counts.no_deadline += 1;
            return WakeBatch(Vec::new());
        };
        let min = if paced {
            earliest
        } else {
            self.sched
                .timed
                .iter()
                .find(|(_, e)| e.role != ParkRole::Backstop)
                .map_or(earliest, |(&(deadline, _), _)| deadline)
        };
        if paced {
            if self.pace_owed(min) > 0 {
                if let Some(t) = &self.sched.pacer_wake
                    && t.id() != std::thread::current().id()
                {
                    t.unpark();
                }
                self.sched.advance_counts.paced_wait += 1;
                return WakeBatch(Vec::new());
            }
        }
        debug_assert!(
            min >= clock.now_nanos(),
            "virtual clock must not move backward"
        );
        clock.store(min);
        self.sched.advance_counts.advances += 1;
        #[cfg(test)]
        self.sched.advance_log.push(min);
        let mut woken: Vec<Wake> = Vec::new();
        while let Some((&(d, _), _)) = self.sched.timed.iter().next() {
            if d > min {
                break;
            }
            if let Some((_, entry)) = self.sched.timed.pop_first() {
                woken.push(entry.wake);
            }
        }
        for (_, wake) in std::mem::take(&mut self.sched.yielders) {
            woken.push(wake);
        }
        self.registry.account_woken(&woken);
        WakeBatch(woken)
    }
}

/// Sync OS-thread wait surface: id mint, parks, sleep, unpark and the
/// cooperative sync yield.
impl FlashInner {
    /// Record the provenance of an engine-backed primitive (its kind + creation
    /// site) under its `cvid`, so the hang dump can label that primitive's parked
    /// waiters. A no-op unless sync tracing is on ([`diag::trace_enabled`]) — the
    /// `bool` check happens BEFORE the `core` lock, so primitive constructors can
    /// call this unconditionally right after [`Self::next_condvar_id`] for ~zero
    /// cost when tracing is off.
    pub(in crate::flash) fn describe_cvid(
        &self,
        cvid: CvId,
        kind: diag::PrimKind,
        loc: &'static Location<'static>,
    ) {
        if !diag::trace_enabled() {
            return;
        }
        let created_on = std::thread::current().name().map(str::to_owned);
        self.core.lock().registry.cv_desc.insert(
            cvid.0,
            CvDesc {
                kind,
                created_on,
                created_at: loc,
            },
        );
    }

    /// Allocate a fresh condvar id (one per [`crate::sync::Condvar`] under sim).
    pub(in crate::flash) fn next_condvar_id(&self) -> CvId {
        self.core.lock().registry.fresh_cv()
    }

    /// Test-harness convenience: park for `Duration` from the current virtual
    /// instant. Reads the clock under the `core` lock and registers the deadline
    /// in the SAME critical section, so no advance can slip between read and
    /// insert. Production parks use [`FlashInner::park_timed_unparkable`];
    /// condvar waits use the `register_*` API.
    #[cfg(test)]
    pub(in crate::flash) fn park_for(&self, d: crate::flash::Duration) {
        let delta = crate::flash::duration_to_nanos(d);
        let token = Token::new();
        let mut s = self.core.lock();
        let deadline = self.clock.now_nanos().saturating_add(delta);
        let id = s.registry.fresh_id();
        s.bound_pace_lag(&self.clock);
        s.sched.timed.insert(
            (deadline, id),
            Entry::timed(WaitKind::Timed, Wake::Sync(Arc::clone(&token))),
        );
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
        token.wait();
        wait.mark_running();
    }

    /// Unparkable thread park: block until the virtual clock reaches `now + d` OR
    /// [`FlashInner::unpark`] targets `thread_id`. Computes the deadline and
    /// registers the entry + `active -= 1` in ONE `core` hold, so no advance can
    /// slip between reading the clock and inserting. A pending `unpark` (one that
    /// arrived while this thread was running) is consumed here and returns
    /// immediately without parking or touching `active`.
    ///
    /// If a wake already landed before parking, returns without touching credit accounting, since
    /// the thread never entered a wait.
    pub(in crate::flash) fn park_timed_unparkable(
        &self,
        d: crate::flash::Duration,
        thread_id: ThreadKey,
        role: ParkRole,
    ) {
        let delta = crate::flash::duration_to_nanos(d);
        let token = Token::new();
        let mut s = self.core.lock();
        if s.sched.unpark_pending.remove(&thread_id) {
            return;
        }
        let deadline = self.clock.now_nanos().saturating_add(delta);
        let id = s.registry.fresh_id();
        s.bound_pace_lag(&self.clock);
        s.sched.timed.insert(
            (deadline, id),
            match role {
                ParkRole::Deadline => {
                    Entry::timed(WaitKind::Thread(thread_id), Wake::Sync(Arc::clone(&token)))
                }
                ParkRole::Backstop => {
                    Entry::backstop(WaitKind::Thread(thread_id), Wake::Sync(Arc::clone(&token)))
                }
            },
        );
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
        token.wait();
        wait.resume();
    }

    /// Virtual `thread::sleep`: block until the virtual clock reaches `now + d`. A
    /// pure timed waiter ([`WaitKind::Timed`]) — woken SOLELY by the engine crossing
    /// its deadline, never by an `unpark` (a `thread::sleep` cannot be cut short,
    /// unlike [`FlashInner::park_timed_unparkable`]). Computes the deadline and
    /// registers the entry + accounts the wait in ONE `core` hold, so no advance
    /// can slip between reading the clock and inserting. Same accounting as
    /// `park_for` — the firer `active += 1`'s the woken entry,
    /// [`FlashInner::resume_after_wait`] settles it.
    pub(in crate::flash) fn sleep_timed(&self, d: crate::flash::Duration) {
        let delta = crate::flash::duration_to_nanos(d);
        let token = Token::new();
        let mut s = self.core.lock();
        let deadline = self.clock.now_nanos().saturating_add(delta);
        let id = s.registry.fresh_id();
        s.bound_pace_lag(&self.clock);
        s.sched.timed.insert(
            (deadline, id),
            Entry::timed(WaitKind::Timed, Wake::Sync(Arc::clone(&token))),
        );
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
        token.wait();
        wait.resume();
    }

    /// Wake a thread parked in [`FlashInner::park_timed_unparkable`]. If it is
    /// currently parked, remove its timed entry, `active += 1`, and fire its
    /// token after releasing the `core` lock. If it is not parked, set
    /// `unpark_pending` so its next park returns at once (mirrors std `unpark`'s
    /// one-token semantics).
    pub(in crate::flash) fn unpark(&self, thread_id: ThreadKey) {
        let mut s = self.core.lock();
        let key = s
            .sched
            .timed
            .iter()
            .find(|(_, e)| e.kind == WaitKind::Thread(thread_id))
            .map(|(&k, _)| k);
        if let Some(key) = key
            && let Some(entry) = s.sched.timed.remove(&key)
        {
            s.registry.account_woken(std::slice::from_ref(&entry.wake));
            drop(s);
            entry.wake.fire();
            return;
        }
        s.sched.unpark_pending.insert(thread_id);
    }

    /// Sim cooperative yield (`thread::yield_now` under `flash`). A busy-poll loop
    /// that calls this RELINQUISHES the engine: it parks as a yield-waiter (dropping
    /// its `active` credit) so the virtual clock can advance past it, and is woken on
    /// the next clock advance to re-check. Without this a spin loop holds
    /// `active != 0` forever, freezing the very clock it polls against — a livelock
    /// (the loop waits for time its own spinning prevents from advancing). Falls
    /// back to a real OS yield ONLY when nothing could ever wake it (no timed
    /// waiter), so a sibling's pure-CPU progress is never blocked behind a wait that
    /// can never fire. Accounting mirrors [`FlashInner::park_timed_unparkable`]:
    /// `enter_wait_locked` drops the credit, the firer re-adds `active`, and the
    /// woken thread `mark_running`s.
    pub(in crate::flash) fn yield_until_advance(&self) {
        let token = Token::new();
        let mut s = self.core.lock();
        if s.sched.timed.is_empty() {
            drop(s);
            std::thread::yield_now();
            return;
        }
        let id = s.registry.fresh_id();
        s.sched.yielders.insert(id, Wake::Sync(Arc::clone(&token)));
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
        token.wait();
        wait.resume();
    }
}

/// Async-yield and condvar waiter surface: registers plus the condvar signal.
impl FlashInner {
    /// Drop path for a [`FlashInner::register_yield_async`] waiter cancelled
    /// before it resolved.
    pub(in crate::flash) fn cancel_yield(&self, id: WaiterId) {
        self.core.lock().sched.yielders.remove(&id);
    }

    /// Register a TIMED condvar waiter (woken by the deadline OR a signal for
    /// `cvid`). The caller holds the DOMAIN guard when calling this (lock order
    /// domain -> core). Accounts the wait via
    /// [`enter_wait_locked`](FlashInner::enter_wait_locked), evaluates the
    /// advance rule, and returns the token to block on plus the advance-due
    /// tokens the caller must fire AFTER releasing the domain guard, plus the
    /// [`WaitGuard`] the caller consumes (`resume()`) once `token.wait()`
    /// returns — the condvar bracket spans modules, so the obligation travels
    /// in the return tuple into the wrapper's off-lock closure.
    pub(in crate::flash) fn register_condvar_timed(
        &self,
        deadline_nanos: u64,
        cvid: CvId,
    ) -> (Arc<Token>, WakeBatch, WaitGuard<'_>) {
        let token = Token::new();
        let mut s = self.core.lock();
        let deadline_nanos = deadline_nanos.max(self.clock.now_nanos());
        let id = s.registry.fresh_id();
        s.bound_pace_lag(&self.clock);
        s.sched.timed.insert(
            (deadline_nanos, id),
            Entry::timed(WaitKind::Condvar(cvid), Wake::Sync(Arc::clone(&token))),
        );
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        (token, adv, wait)
    }

    /// Register an UNTIMED condvar waiter (no deadline; woken only by a signal for
    /// `cvid`). Same return shape and accounting as [`FlashInner::register_condvar_timed`].
    pub(in crate::flash) fn register_condvar_untimed(
        &self,
        cvid: CvId,
    ) -> (Arc<Token>, WakeBatch, WaitGuard<'_>) {
        let token = Token::new();
        let parked = Parked::here();
        let mut s = self.core.lock();
        let id = s.registry.fresh_id();
        s.sched.indef.insert(
            id,
            Entry::indef(
                WaitKind::Condvar(cvid),
                Wake::Sync(Arc::clone(&token)),
                parked,
            ),
        );
        let wait = self.enter_wait_locked(&mut s);
        let adv = s.try_advance(&self.clock);
        drop(s);
        (token, adv, wait)
    }

    /// Register an ASYNC cooperative-yield waiter (sim `tokio::task::yield_now`).
    /// Parks the task as a yield-waiter (woken on the next clock advance), then runs
    /// the advance rule and returns its id + granted flag + the [`WakeBatch`] the caller
    /// fires (mirroring the sibling `register_*_async` registers). This is the sim
    /// analogue of a real `yield_now`: real time passes while a task yields, so here
    /// the task releases its `active_async` slot (the spawn gate does so when the
    /// yield future returns Pending) and the virtual clock is free to advance to the
    /// next event. Still NO resolve-at-once: the waiter is inserted parked, and the
    /// returned advance only GRANTS it on a quiescent edge (`active == active_async
    /// == 0`) — under a participated poll the task is still running (`active_async >
    /// 0`) so the advance is a no-op and the gate park does the real advance, exactly
    /// as before. The grant (here, the lone-yield rescue, or a later clock advance)
    /// sets `granted` and the waker re-polls. The fired advance unwedges a
    /// genuinely-quiescent non-participated `block_on` whose only `.await` is a yield.
    pub(in crate::flash) fn register_yield_async(
        &self,
        waker: Waker,
    ) -> (WaiterId, Arc<AtomicBool>, WakeBatch) {
        let granted = Arc::new(AtomicBool::new(false));
        let mut s = self.core.lock();
        let id = s.registry.fresh_id();
        s.sched.yielders.insert(
            id,
            Wake::Task {
                waker,
                granted: Arc::clone(&granted),
                task: ctx::cur_async(),
            },
        );
        let adv = s.try_advance(&self.clock);
        drop(s);
        (id, granted, adv)
    }

    /// Wake condvar waiters for `cvid`: `all == true` wakes every matching waiter
    /// (`notify_all`), `all == false` wakes at most one (`notify_one`). Scans BOTH
    /// `timed` and `indef`, removes each woken entry, `active += 1` per woken
    /// entry, and fires their tokens after releasing the `core` lock.
    pub(in crate::flash) fn signal_condvar(&self, cvid: CvId, all: bool) {
        let mut s = self.core.lock();
        let timed_keys: Vec<(u64, WaiterId)> = s
            .sched
            .timed
            .iter()
            .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
            .map(|(&k, _)| k)
            .collect();
        let indef_keys: Vec<WaiterId> = s
            .sched
            .indef
            .iter()
            .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
            .map(|(&k, _)| k)
            .collect();

        let mut woken: Vec<Wake> = Vec::new();
        for key in timed_keys {
            if !all && !woken.is_empty() {
                break;
            }
            if let Some(entry) = s.sched.timed.remove(&key) {
                woken.push(entry.wake);
            }
        }
        for key in indef_keys {
            if !all && !woken.is_empty() {
                break;
            }
            if let Some(entry) = s.sched.indef.remove(&key) {
                woken.push(entry.wake);
            }
        }
        s.registry.account_woken(&woken);
        drop(s);
        for t in woken {
            t.fire();
        }
    }
}

/// Async sleep/notify waiter registers.
impl FlashInner {
    /// Register an UNTIMED async `Notify` waiter for `cvid`, OR consume a stored
    /// permit. Returns `(handle, to_wake)`; `handle` is `None` when a permit was
    /// waiting (the caller resolves at once without parking).
    pub(in crate::flash) fn register_notify_async(
        &self,
        cvid: CvId,
        waker: Waker,
    ) -> (Option<AsyncHandle>, WakeBatch) {
        let granted = Arc::new(AtomicBool::new(false));
        let parked = Parked::here();
        let mut s = self.core.lock();
        if s.sched.notify_permits.remove(&cvid) {
            return (None, WakeBatch(Vec::new()));
        }
        let id = s.registry.fresh_id();
        s.sched.indef.insert(
            id,
            Entry::indef(
                WaitKind::Condvar(cvid),
                Wake::Task {
                    waker,
                    granted: Arc::clone(&granted),
                    task: ctx::cur_async(),
                },
                parked,
            ),
        );
        let adv = s.try_advance(&self.clock);
        drop(s);
        (
            Some(AsyncHandle {
                granted,
                timed_key: None,
                indef_key: Some(id),
            }),
            adv,
        )
    }

    /// Register a TIMED async sleep waiter `delta_nanos` from the CURRENT virtual
    /// instant, then run the advance rule. The deadline is computed from the clock
    /// read INSIDE the lock (so no advance can slip between reading the clock and
    /// inserting — a deadline is therefore never below the current clock, the
    /// "no backward" invariant). Registration touches no counter: the task is
    /// already counted in `active_async` by its poll-wrapper for the current poll,
    /// and the waiter is removed by [`FlashInner::cancel_async_wait`] if the future
    /// is dropped before it fires.
    pub(in crate::flash) fn register_sleep_async(
        &self,
        delta_nanos: u64,
        waker: Waker,
    ) -> (AsyncHandle, WakeBatch) {
        let granted = Arc::new(AtomicBool::new(false));
        let mut s = self.core.lock();
        let deadline_nanos = self.clock.now_nanos().saturating_add(delta_nanos);
        let id = s.registry.fresh_id();
        let key = (deadline_nanos, id);
        s.bound_pace_lag(&self.clock);
        s.sched.timed.insert(
            key,
            Entry::timed(
                WaitKind::Timed,
                Wake::Task {
                    waker,
                    granted: Arc::clone(&granted),
                    task: ctx::cur_async(),
                },
            ),
        );
        let adv = s.try_advance(&self.clock);
        drop(s);
        (
            AsyncHandle {
                granted,
                timed_key: Some(key),
                indef_key: None,
            },
            adv,
        )
    }
}

/// Async task slot accounting: the spawn-time acquire plus the gate FSM
/// transitions coupled to the counter under the `core` lock.
impl FlashInner {
    /// Acquire an `active_async` slot for a RUNNABLE async task queued to be polled
    /// (just spawned). Adding a participant can never enable an advance, so this does
    /// NOT run the advance rule. Called once by [`participate`](crate::flash::participate) at
    /// construction; the PARKED→RUNNABLE wake re-acquire goes through
    /// [`FlashInner::gate_wake_parked`], which couples the acquire to the state CAS
    /// under the lock.
    /// Returns the task's gate, whose [`TaskDiag`] the registry keeps a second
    /// handle to until the task completes or drops.
    pub(in crate::flash) fn async_acquire(&self, loc: &'static Location<'static>) -> Arc<TaskGate> {
        let mut s = self.core.lock();
        let id = s.registry.next_task_id;
        s.registry.next_task_id += 1;
        s.registry.active_async += 1;
        s.registry.active_async_holders.insert(id, loc);
        let gate = TaskGate::new(id, loc, Arc::new(TaskDiag::default()));
        s.registry.task_diag.insert(id, gate.diag());
        gate
    }

    /// Gate completion under the `core` lock: mark `Done` and release the slot
    /// atomically (mirrors [`FlashInner::gate_park`]'s release arm for a poll that
    /// returned Ready).
    pub(super) fn gate_complete(&self, state: &AtomicTaskState, id: u64) {
        let mut s = self.core.lock();
        state.store(TaskState::Done);
        s.registry.active_async_holders.remove(&id);
        s.registry.task_diag.remove(&id);
        let adv = s.release_async(&self.clock);
        drop(s);
        adv.fire();
    }

    /// Gate drop under the `core` lock: swap to `Done`; release the slot iff the task
    /// still held one (its prior state was counted — `Runnable`/`Running`/`RunningNotified`).
    pub(super) fn gate_drop_release(&self, state: &AtomicTaskState, id: u64) {
        let mut s = self.core.lock();
        s.registry.task_diag.remove(&id);
        match state.swap(TaskState::Done) {
            TaskState::Runnable | TaskState::Running | TaskState::RunningNotified => {
                s.registry.active_async_holders.remove(&id);
                let adv = s.release_async(&self.clock);
                drop(s);
                adv.fire();
            }
            TaskState::Parked | TaskState::Done => {}
        }
    }

    /// Gate park under the `core` lock: atomically CAS `Running`→`Parked` and release
    /// the async slot, or — a wake landed mid-poll, so the state is `RunningNotified`
    /// and the CAS fails ([`ParkOutcome::WokenMidPoll`]) — store `Runnable`, keeping
    /// the slot for the re-poll the wake already scheduled. Holding the lock across
    /// the CAS and the counter update is what closes the wake→poll race: a concurrent
    /// [`FlashInner::gate_wake_parked`] acquire can no longer interleave between a
    /// lock-free CAS and a separately-locked release, which would leave a re-runnable
    /// task uncounted and over-release on its next park. Returns the fully-handled
    /// outcome (callers need no further action on either arm).
    pub(super) fn gate_park(&self, state: &AtomicTaskState, id: u64) -> ParkOutcome {
        let mut s = self.core.lock();
        if state.compare_exchange(TaskState::Running, TaskState::Parked) {
            s.registry.active_async_holders.remove(&id);
            let adv = s.release_async(&self.clock);
            drop(s);
            adv.fire();
            ParkOutcome::Parked
        } else {
            state.store(TaskState::Runnable);
            ParkOutcome::WokenMidPoll
        }
    }

    /// Wake a PARKED gate under the `core` lock: CAS `Parked`→`Runnable` and acquire
    /// the slot atomically. Returns [`WakeOutcome::Resumed`] when it transitioned
    /// (the caller then forwards the runtime waker), [`WakeOutcome::NotParked`] if
    /// the state was not `Parked` (the caller's wake loop re-reads and handles the
    /// current state lock-free). Coupling the acquire to the CAS under the lock is
    /// the counterpart to [`FlashInner::gate_park`]'s release.
    pub(super) fn gate_wake_parked(
        &self,
        state: &AtomicTaskState,
        id: u64,
        loc: &'static Location<'static>,
    ) -> WakeOutcome {
        let mut s = self.core.lock();
        if state.compare_exchange(TaskState::Parked, TaskState::Runnable) {
            s.registry.active_async += 1;
            s.registry.active_async_holders.insert(id, loc);
            WakeOutcome::Resumed
        } else {
            WakeOutcome::NotParked
        }
    }
}

/// Async waiter cancel plus the notify/channel signal surface.
impl FlashInner {
    /// Drop path for an async waiter future cancelled before it resolved (e.g. it
    /// lost a `tokio::select!`). Just remove its still-parked entry, if any. Async
    /// waiters never hold a counter slot (the firer does not bump `active_async` —
    /// the poll-wrapper owns that count per-poll), so there is nothing to release:
    /// the surrounding task stays counted by its poll-wrapper either way.
    pub(in crate::flash) fn cancel_async_wait(&self, handle: &AsyncHandle) {
        let mut s = self.core.lock();
        match (handle.timed_key, handle.indef_key) {
            (Some(key), _) => {
                s.sched.timed.remove(&key);
            }
            (_, Some(id)) => {
                s.sched.indef.remove(&id);
            }
            _ => {}
        }
    }

    /// Register an UNTIMED async channel waiter for `cvid`. Unlike
    /// [`FlashInner::register_notify_async`] this NEVER consumes a permit: a sim
    /// channel (`tokio::sync::mpsc`/`oneshot`) holds its own queue as the source of
    /// truth, so the engine waiter is a pure wakeup with no permit bookkeeping. The
    /// caller must register WHILE holding the channel's own queue lock so a
    /// concurrent producer cannot signal-with-no-waiter between the empty-check and
    /// the park (no lost wakeup). Returns `(handle, advance)`; fire the advance
    /// after dropping the queue lock.
    pub(in crate::flash) fn register_channel_async(
        &self,
        cvid: CvId,
        waker: Waker,
    ) -> (AsyncHandle, WakeBatch) {
        let granted = Arc::new(AtomicBool::new(false));
        let parked = Parked::here();
        let mut s = self.core.lock();
        let id = s.registry.fresh_id();
        s.sched.indef.insert(
            id,
            Entry::indef(
                WaitKind::Condvar(cvid),
                Wake::Task {
                    waker,
                    granted: Arc::clone(&granted),
                    task: ctx::cur_async(),
                },
                parked,
            ),
        );
        let adv = s.try_advance(&self.clock);
        drop(s);
        (
            AsyncHandle {
                granted,
                timed_key: None,
                indef_key: Some(id),
            },
            adv,
        )
    }

    /// Signal a sim channel `cvid`: wake one (`all == false`) or every
    /// (`all == true`) parked waiter. Unlike [`FlashInner::signal_notify`] it stores
    /// NO permit when none is parked — the channel's queue already holds the
    /// produced item (or the closed flag), and the next receiver/sender poll
    /// re-checks that queue (or the live-count) directly, so a missed signal is
    /// harmless. `all` is for the close edges (a dropped receiver wakes every
    /// blocked sender; a dropped last sender wakes the receiver).
    pub(in crate::flash) fn signal_channel(&self, cvid: CvId, all: bool) {
        let mut s = self.core.lock();
        let keys: Vec<WaiterId> = s
            .sched
            .indef
            .iter()
            .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
            .map(|(&k, _)| k)
            .collect();
        let mut woken: Vec<Wake> = Vec::new();
        for key in keys {
            if !all && !woken.is_empty() {
                break;
            }
            if let Some(entry) = s.sched.indef.remove(&key) {
                woken.push(entry.wake);
            }
        }
        if woken.is_empty() {
            return;
        }
        s.registry.account_woken(&woken);
        drop(s);
        for w in woken {
            w.fire();
        }
    }

    /// Signal an async `Notify` for `cvid`: wake at most one waiter; if none are
    /// parked, store a permit so the next `notified()` resolves at once (tokio
    /// `notify_one` semantics).
    pub(in crate::flash) fn signal_notify(&self, cvid: CvId) {
        let mut s = self.core.lock();
        let woken_key = s
            .sched
            .indef
            .iter()
            .find(|(_, e)| e.kind == WaitKind::Condvar(cvid))
            .map(|(&k, _)| k);
        let mut woken: Vec<Wake> = Vec::new();
        if let Some(key) = woken_key
            && let Some(entry) = s.sched.indef.remove(&key)
        {
            woken.push(entry.wake);
        }
        if woken.is_empty() {
            s.sched.notify_permits.insert(cvid);
        } else {
            s.registry.account_woken(&woken);
        }
        drop(s);
        for t in woken {
            t.fire();
        }
    }
}

/// Handle an async waiter future holds for the lifetime of one park. Carries the
/// engine key so a cancelled (dropped-before-resolve) future can remove its
/// still-parked entry, and the `granted` flag the firer sets so the future can
/// tell a real wake from a cancel and balance `active` exactly.
pub(crate) struct AsyncHandle {
    granted: Arc<AtomicBool>,
    indef_key: Option<WaiterId>,
    timed_key: Option<(u64, WaiterId)>,
}

impl AsyncHandle {
    /// True once the engine (or a signal) selected this waiter. The future
    /// resolves `Ready` on its next poll. Counting is GRANT-driven (only the
    /// firer sets this), so a clock jump via some OTHER advance never resolves
    /// the waiter early. No counter is touched on resolve — the task's
    /// `active_async` slot is owned by the spawn poll-wrapper.
    pub(crate) fn granted(&self) -> bool {
        self.granted.load(Ordering::Acquire)
    }
}

/// Test-only coordinator that holds a RUNNING slot in `active` for its lifetime,
/// so the engine cannot advance until it is dropped. The harness uses it to
/// batch many waiters: hold it, let every worker register + park (each park
/// bootstraps to `Parked` without touching `active`), then drop it as the single
/// `active -> 0` edge so the advance sees the full multiset at once.
///
/// Production has NO coordinator: a real root bootstraps lazily on its first
/// wrapped wait. This exists only to make the harness's deliberately-batched
/// scenarios deterministic; it is the test-side stand-in for the old explicit
/// `register_participant`. Borrows its engine instance, so it works on local
/// instances too.
#[cfg(test)]
#[must_use]
pub(crate) struct TestHold<'a> {
    flash: &'a FlashInner,
}

/// The async counterpart: a task occupying its `active_async` slot across an
/// await, the way one mid-flight over a real I/O op does. Production acquires
/// the slot through the spawn gate, which is wired to the process engine, so a
/// pure-scheduler test on a LOCAL instance has no other way to express a held
/// slot.
#[cfg(test)]
#[must_use]
pub(crate) struct TestHoldAsync<'a> {
    flash: &'a FlashInner,
}

#[cfg(test)]
impl FlashInner {
    pub(in crate::flash) fn advance_log(&self) -> Vec<u64> {
        self.core.lock().sched.advance_log.clone()
    }

    /// Test-only: number of currently parked cooperative-yield waiters.
    pub(in crate::flash) fn diag_yield_count(&self) -> usize {
        self.core.lock().sched.yielders.len()
    }

    /// Test-only: number of currently parked untimed waiters.
    pub(in crate::flash) fn indef_count(&self) -> usize {
        self.core.lock().sched.indef.len()
    }

    pub(in crate::flash) fn test_hold(&self) -> TestHold<'_> {
        self.core.lock().registry.active += 1;
        TestHold { flash: self }
    }

    pub(in crate::flash) fn test_hold_async(&self) -> TestHoldAsync<'_> {
        self.core.lock().registry.active_async += 1;
        TestHoldAsync { flash: self }
    }

    /// Test-only: number of currently parked timed waiters.
    pub(in crate::flash) fn timed_count(&self) -> usize {
        self.core.lock().sched.timed.len()
    }

    delegate::delegate! {
        to self.core {
            /// Test-only: number of currently RUNNING participants.
            #[expr($.registry.active)]
            #[call(lock)]
            pub(in crate::flash) fn active_count(&self) -> usize;
            /// Test-only: number of async tasks the engine currently counts as
            /// non-quiescent (runnable or running). A task woken but not yet re-polled MUST
            /// be counted here, or the clock can advance past it.
            #[expr($.registry.active_async)]
            #[call(lock)]
            pub(in crate::flash) fn async_active_count(&self) -> usize;
        }
    }
}

#[cfg(test)]
impl Drop for TestHold<'_> {
    fn drop(&mut self) {
        let mut s = self.flash.core.lock();
        debug_assert!(s.registry.active > 0, "TestHold drop without matching hold");
        s.registry.active -= 1;
        let adv = s.try_advance(&self.flash.clock);
        drop(s);
        adv.fire();
    }
}

#[cfg(test)]
impl Drop for TestHoldAsync<'_> {
    fn drop(&mut self) {
        let mut s = self.flash.core.lock();
        debug_assert!(
            s.registry.active_async > 0,
            "TestHoldAsync drop without matching hold"
        );
        s.registry.active_async -= 1;
        let adv = s.try_advance(&self.flash.clock);
        drop(s);
        adv.fire();
    }
}
