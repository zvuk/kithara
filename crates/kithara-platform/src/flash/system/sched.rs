use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    task::Waker,
};

use parking_lot::Mutex;
use web_time::Instant as RealInstant;

#[cfg(test)]
use super::credit::mark_running;
use super::{
    credit::{enter_wait_locked, resume_after_wait},
    wake::{Token, Wake},
};
use crate::flash::SIM_NANOS;

/// What kind of waiter an [`Entry`] is, so a signal targets the right group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitKind {
    /// A timed waiter with no early-wake channel: woken solely by the engine
    /// crossing its deadline. Used by the test harness `park_for` and by the
    /// async `sleep` future (`register_sleep_async`).
    Timed,
    /// An unparkable thread park (`park_timeout`): woken by the deadline OR by
    /// [`unpark`] targeting this thread id.
    Thread(u64),
    /// A condvar waiter: woken by the deadline (when timed) OR by
    /// [`signal_condvar`] for this condvar id.
    Condvar(u64),
}

/// A parked waiter's wake handle plus the group it belongs to.
struct Entry {
    wake: Wake,
    kind: WaitKind,
}

/// Process-global quiescence scheduler. ALL fields mutate only under this lock.
/// `SIM_NANOS` is written only by `try_advance_locked` (and the test-only
/// additive `advance`, which the harness never calls).
pub(super) struct SimSched {
    /// SYNC participants currently RUNNING (OS threads not inside a wrapped
    /// `park_timeout`/`Condvar` wait). Bumped by the firer on wake (real OS
    /// scheduling latency must be covered), decremented at the next wait /
    /// thread exit via the thread-local `Credit` bracket (`credit` module).
    pub(super) active: usize,
    /// ASYNC tasks the engine counts as NON-QUIESCENT. A task is counted from the
    /// moment it becomes RUNNABLE — spawned, or woken (its waker fired and it is
    /// queued to be polled) — until it next PARKS (poll returns `Pending` with no
    /// pending re-wake), completes, or is dropped. Maintained by the spawn
    /// poll-wrapper's per-task gate via [`async_acquire`]/[`async_release`], NOT
    /// the firer. Counting a woken-but-not-yet-repolled task (not merely a
    /// mid-poll one) is what stops the clock jumping past a runnable task — it
    /// closes the wake→poll window. The engine may advance only when BOTH
    /// `active` and `active_async` are zero.
    pub(super) active_async: usize,
    /// Timed waiters keyed by (virtual deadline nanos, unique id) so the
    /// minimum deadline is `first_key_value`; ties share the same deadline and
    /// differ only by id. The map doubles as entry storage.
    parked_timed: BTreeMap<(u64, u64), Entry>,
    /// Untimed waiters (no deadline) keyed by unique id; woken only by a signal
    /// (`signal_condvar`), never by a clock jump.
    parked_indef: BTreeMap<u64, Entry>,
    /// Thread ids whose `unpark` arrived while not parked: the next
    /// `park_timed_unparkable` for that id consumes the flag and returns at once.
    unpark_pending: BTreeSet<u64>,
    /// Condvar ids whose `notify_one` arrived while no waiter
    /// was registered: the next `notified()` first-poll for that cvid consumes
    /// the permit and resolves immediately (mirrors `tokio::Notify`'s stored
    /// permit). Only set by the async [`Notify`](crate::sync::Notify) path.
    notify_permit: BTreeSet<u64>,
    /// Cooperative-yield waiters (sim `thread::yield_now` AND
    /// `tokio::task::yield_now`): a busy-poll loop that relinquishes the engine so
    /// the clock can advance. They carry NO deadline — woken on the NEXT clock
    /// advance, then re-check (the loop's own deadline / poll condition is
    /// re-evaluated on re-poll). A `Sync` entry is a parked OS thread; a `Task`
    /// entry is a parked async task whose `active_async` slot the spawn gate
    /// releases while it waits. Keyed by id.
    yield_waiters: BTreeMap<u64, Wake>,
    /// REAL I/O operations currently in flight (socket sends / body-chunk
    /// awaits bracketed by [`crate::flash::RealIoScope`]). While non-zero the clock
    /// is PACED, not pinned: [`try_advance_locked`] refuses any jump beyond
    /// `pace_anchor`'s virtual base plus the REAL time elapsed since it was
    /// set — virtual time may not outrun real time while real-world transit
    /// is pending. Maintained by `pace::real_io_enter`/`real_io_exit`.
    pub(super) real_io: usize,
    /// `(real instant, virtual nanos)` sampled when `real_io` went 0 -> 1.
    /// The pace limit is `anchor_virtual + (real_now - anchor_real)`; cleared
    /// when the last op completes so full-speed collapse resumes at once.
    pub(super) pace_anchor: Option<(RealInstant, u64)>,
    next_id: u64,
    next_cvid: u64,
    /// Test-only oracle: the sequence of `SIM_NANOS` values the engine jumped
    /// to, recorded under this lock alongside each advance. Proves min-jump,
    /// tie-batching (fewer entries than waiters), and determinism (identical
    /// sequence across runs). Cleared by [`reset`]; not present in non-test
    /// builds.
    #[cfg(test)]
    advance_log: Vec<u64>,
}

impl SimSched {
    const fn new() -> Self {
        Self {
            active: 0,
            active_async: 0,
            parked_timed: BTreeMap::new(),
            parked_indef: BTreeMap::new(),
            unpark_pending: BTreeSet::new(),
            notify_permit: BTreeSet::new(),
            yield_waiters: BTreeMap::new(),
            real_io: 0,
            pace_anchor: None,
            next_id: 0,
            next_cvid: 0,
            #[cfg(test)]
            advance_log: Vec::new(),
        }
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

pub(super) static SCHED: Mutex<SimSched> = Mutex::new(SimSched::new());

/// Result of an advance attempt: the wakes to fire after releasing `SCHED`.
/// Firing is always immediate (pure-virtual collapse at full speed).
pub(crate) struct Advance {
    wakes: Vec<Wake>,
}

/// Evaluate the advance rule while holding `SCHED`. Returns the [`Advance`] whose
/// `wakes` the caller fires AFTER releasing the lock (firing under the lock would
/// make the woken thread immediately contend on SCHED) via [`fire_advance`].
/// Operates only on `parked_timed` — it never fires `parked_indef`, which has no
/// deadline. Fires nothing unless every participant is parked (`active == 0`) and
/// at least one timed waiter exists.
pub(super) fn try_advance_locked(s: &mut SimSched) -> Advance {
    if s.active != 0 || s.active_async != 0 {
        // A running participant (sync OS thread OR async task mid-poll): do not
        // jump. Both counters must be zero for genuine quiescence.
        return Advance { wakes: Vec::new() };
    }
    // Cooperative yielders re-poll at the CURRENT instant before the clock can
    // advance to a `Thread` (`park_timeout`) deadline. Such a park is
    // event-driven — woken by an `unpark` from a peer's progress — and its
    // deadline is only a watchdog fallback. A pending yielder is an active
    // participant (e.g. the audio worker mid-produce) that may unpark it, so it
    // MUST get a chance to make progress before the clock jumps; otherwise the
    // jump fires the park's watchdog prematurely while the producer still had
    // work to flush (the audio worker/reader saturation deadlock). A REAL timer
    // (`Timed`/`Condvar`) could be what a yielder awaits, so when one is the
    // earliest waiter the clock still advances (below). Vacuously true when
    // `parked_timed` is empty (the no-timer yielder-drain case).
    if !s.yield_waiters.is_empty()
        && s.parked_timed
            .values()
            .all(|e| matches!(e.kind, WaitKind::Thread(_)))
    {
        let woken: Vec<Wake> = std::mem::take(&mut s.yield_waiters).into_values().collect();
        s.active += woken.iter().filter(|w| !w.is_task()).count();
        for w in &woken {
            w.mark_granted_under_lock();
        }
        return Advance { wakes: woken };
    }
    let Some((&(min, _), _)) = s.parked_timed.iter().next() else {
        // Quiescent with NO timed waiter to advance to. Any parked yield-waiter
        // was already drained above (when `parked_timed` is empty the all-`Thread`
        // guard is vacuously true), so reaching here means there is nothing
        // runnable at all: stay put.
        return Advance { wakes: Vec::new() };
    };
    if s.real_io != 0
        && let Some((anchor_real, anchor_virtual)) = s.pace_anchor
    {
        // PACE while real I/O is in flight: virtual time may not outrun real
        // time, so the earliest deadline fires only once the equivalent REAL
        // time has accrued since the first in-flight op anchored the pace.
        // Deferred advances are re-attempted by the pace tick (`pace.rs`).
        let elapsed = u64::try_from(anchor_real.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if min > anchor_virtual.saturating_add(elapsed) {
            return Advance { wakes: Vec::new() };
        }
    }
    debug_assert!(
        min >= SIM_NANOS.load(Ordering::Acquire),
        "virtual clock must not move backward"
    );
    SIM_NANOS.store(min, Ordering::Release);
    #[cfg(test)]
    s.advance_log.push(min);
    let mut woken = Vec::new();
    while let Some((&(d, _), _)) = s.parked_timed.iter().next() {
        if d != min {
            break;
        }
        if let Some((_, entry)) = s.parked_timed.pop_first() {
            woken.push(entry.wake);
        }
    }
    // A clock advance is progress: wake every cooperative-yield waiter to
    // re-check its poll condition. They carry no deadline, so an advance is the
    // only thing that reschedules them. Sync entries are `active`-bumped +
    // `mark_granted`'d by the loops below (like timed wakes); Task entries
    // re-acquire their `active_async` slot via the gate's waker.
    for (_, wake) in std::mem::take(&mut s.yield_waiters) {
        woken.push(wake);
    }
    // Pre-increment `active` ONLY for sync OS-thread wakes, so a woken thread
    // does NOT re-increment on return — it just resumes running (the bump covers
    // its real OS wake latency). Async-task wakes are NOT bumped here: the spawn
    // poll-wrapper counts them in `active_async` when they are next polled. All
    // wakes are marked granted under the lock so a racing cancel is consistent.
    s.active += woken.iter().filter(|w| !w.is_task()).count();
    for w in &woken {
        w.mark_granted_under_lock();
    }
    Advance { wakes: woken }
}

/// Fire an [`Advance`]'s wakes after `SCHED` is released. Firing is immediate:
/// the advance chain runs at full virtual speed.
pub(crate) fn fire_advance(adv: Advance) {
    for w in adv.wakes {
        w.fire();
    }
}

/// Allocate a fresh condvar id (one per [`crate::sync::Condvar`] under sim).
pub(crate) fn next_condvar_id() -> u64 {
    let mut s = SCHED.lock();
    let id = s.next_cvid;
    s.next_cvid += 1;
    id
}

/// Test-harness convenience: park for `Duration` from the current virtual
/// instant. Reads the clock under `SCHED` and registers the deadline in the SAME
/// critical section, so no advance can slip between read and insert. Production
/// parks use [`park_timed_unparkable`]; condvar waits use the `register_*` API.
#[cfg(test)]
pub(crate) fn park_for(d: crate::flash::Duration) {
    let delta = crate::flash::duration_to_nanos(d);
    let token = Token::new();
    let mut s = SCHED.lock();
    let deadline = SIM_NANOS.load(Ordering::Acquire).saturating_add(delta);
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline, id),
        Entry {
            wake: Wake::Sync(Arc::clone(&token)),
            kind: WaitKind::Timed,
        },
    );
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    fire_advance(adv);
    token.wait();
    mark_running();
}

/// Unparkable thread park: block until the virtual clock reaches `now + d` OR
/// [`unpark`] targets `thread_id`. Computes the deadline and registers the
/// entry + `active -= 1` in ONE `SCHED` hold, so no advance can slip between
/// reading the clock and inserting. A pending `unpark` (one that arrived while
/// this thread was running) is consumed here and returns immediately without
/// parking or touching `active`.
pub(crate) fn park_timed_unparkable(d: crate::flash::Duration, thread_id: u64) {
    let delta = crate::flash::duration_to_nanos(d);
    let token = Token::new();
    let mut s = SCHED.lock();
    if s.unpark_pending.remove(&thread_id) {
        // A wake already landed: do not park (and do not touch credit — we
        // never entered a wait, so the thread stays as it was).
        return;
    }
    let deadline = SIM_NANOS.load(Ordering::Acquire).saturating_add(delta);
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline, id),
        Entry {
            wake: Wake::Sync(Arc::clone(&token)),
            kind: WaitKind::Thread(thread_id),
        },
    );
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    fire_advance(adv);
    token.wait();
    resume_after_wait();
}

/// Virtual `thread::sleep`: block until the virtual clock reaches `now + d`. A
/// pure timed waiter ([`WaitKind::Timed`]) — woken SOLELY by the engine crossing
/// its deadline, never by an `unpark` (a `thread::sleep` cannot be cut short,
/// unlike [`park_timed_unparkable`]). Computes the deadline and registers the
/// entry + accounts the wait in ONE `SCHED` hold, so no advance can slip between
/// reading the clock and inserting. Same accounting as [`park_for`] — the firer
/// `active += 1`'s the woken entry, [`resume_after_wait`] settles it.
pub(crate) fn sleep_timed(d: crate::flash::Duration) {
    let delta = crate::flash::duration_to_nanos(d);
    let token = Token::new();
    let mut s = SCHED.lock();
    let deadline = SIM_NANOS.load(Ordering::Acquire).saturating_add(delta);
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline, id),
        Entry {
            wake: Wake::Sync(Arc::clone(&token)),
            kind: WaitKind::Timed,
        },
    );
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    fire_advance(adv);
    token.wait();
    resume_after_wait();
}

/// Wake a thread parked in [`park_timed_unparkable`]. If it is currently parked,
/// remove its timed entry, `active += 1`, and fire its token after releasing
/// `SCHED`. If it is not parked, set `unpark_pending` so its next park returns
/// at once (mirrors std `unpark`'s one-token semantics).
pub(crate) fn unpark(thread_id: u64) {
    let mut s = SCHED.lock();
    let key = s
        .parked_timed
        .iter()
        .find(|(_, e)| e.kind == WaitKind::Thread(thread_id))
        .map(|(&k, _)| k);
    if let Some(key) = key
        && let Some(entry) = s.parked_timed.remove(&key)
    {
        s.active += 1;
        drop(s);
        entry.wake.fire();
        return;
    }
    s.unpark_pending.insert(thread_id);
}

/// Sim cooperative yield (`thread::yield_now` under `flash`). A busy-poll loop
/// that calls this RELINQUISHES the engine: it parks as a yield-waiter (dropping
/// its `active` credit) so the virtual clock can advance past it, and is woken on
/// the next clock advance to re-check. Without this a spin loop holds
/// `active != 0` forever, freezing the very clock it polls against — a livelock
/// (the loop waits for time its own spinning prevents from advancing). Falls
/// back to a real OS yield ONLY when nothing could ever wake it (no timed
/// waiter), so a sibling's pure-CPU progress is never blocked behind a wait that
/// can never fire. Accounting mirrors [`park_timed_unparkable`]:
/// `enter_wait_locked` drops the credit, the firer re-adds `active`, and the
/// woken thread `mark_running`s.
pub(crate) fn yield_until_advance() {
    let token = Token::new();
    let mut s = SCHED.lock();
    if s.parked_timed.is_empty() {
        drop(s);
        std::thread::yield_now();
        return;
    }
    let id = s.fresh_id();
    s.yield_waiters.insert(id, Wake::Sync(Arc::clone(&token)));
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    fire_advance(adv);
    token.wait();
    resume_after_wait();
}

/// Register an ASYNC cooperative-yield waiter (sim `tokio::task::yield_now`).
/// Parks the task as a yield-waiter (woken on the next clock advance), then runs
/// the advance rule and returns its id + granted flag + the [`Advance`] the caller
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
pub(crate) fn register_yield_async(waker: Waker) -> (u64, Arc<AtomicBool>, Advance) {
    let granted = Arc::new(AtomicBool::new(false));
    let mut s = SCHED.lock();
    let id = s.fresh_id();
    s.yield_waiters.insert(
        id,
        Wake::Task {
            waker,
            granted: Arc::clone(&granted),
        },
    );
    let adv = try_advance_locked(&mut s);
    drop(s);
    (id, granted, adv)
}

/// Drop path for a [`register_yield_async`] waiter cancelled before it resolved.
pub(crate) fn cancel_yield(id: u64) {
    SCHED.lock().yield_waiters.remove(&id);
}

/// Register a TIMED condvar waiter (woken by the deadline OR a signal for
/// `cvid`). The caller holds the DOMAIN guard when calling this (lock order
/// domain -> SCHED). Accounts the wait via [`enter_wait_locked`], evaluates the
/// advance rule, and returns the token to block on plus the advance-due tokens
/// the caller must fire AFTER releasing the domain guard. The caller calls
/// [`mark_running_after_condvar`](super::credit::mark_running_after_condvar)
/// once `token.wait()` returns.
pub(crate) fn register_condvar_timed(deadline_nanos: u64, cvid: u64) -> (Arc<Token>, Advance) {
    let token = Token::new();
    let mut s = SCHED.lock();
    // The caller computed `deadline_nanos` from `Instant::now()` OUTSIDE this
    // lock; an async sleep could have jumped the clock since, leaving the
    // deadline below the current virtual instant. Clamp to "now" under the lock
    // so the monotonic-clock invariant holds (a wait whose virtual timeout has
    // already elapsed fires on the next advance, and the caller re-checks its
    // predicate). This enforces "no backward clock" atomically at the single
    // registration chokepoint.
    let deadline_nanos = deadline_nanos.max(SIM_NANOS.load(Ordering::Acquire));
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline_nanos, id),
        Entry {
            wake: Wake::Sync(Arc::clone(&token)),
            kind: WaitKind::Condvar(cvid),
        },
    );
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    (token, adv)
}

/// Register an UNTIMED condvar waiter (no deadline; woken only by a signal for
/// `cvid`). Same return shape and accounting as [`register_condvar_timed`].
pub(crate) fn register_condvar_untimed(cvid: u64) -> (Arc<Token>, Advance) {
    let token = Token::new();
    let mut s = SCHED.lock();
    let id = s.fresh_id();
    s.parked_indef.insert(
        id,
        Entry {
            wake: Wake::Sync(Arc::clone(&token)),
            kind: WaitKind::Condvar(cvid),
        },
    );
    enter_wait_locked(&mut s);
    let adv = try_advance_locked(&mut s);
    drop(s);
    (token, adv)
}

/// Wake condvar waiters for `cvid`: `all == true` wakes every matching waiter
/// (`notify_all`), `all == false` wakes at most one (`notify_one`). Scans BOTH
/// `parked_timed` and `parked_indef`, removes each woken entry, `active += 1`
/// per woken entry, and fires their tokens after releasing `SCHED`.
pub(crate) fn signal_condvar(cvid: u64, all: bool) {
    let mut s = SCHED.lock();
    let timed_keys: Vec<(u64, u64)> = s
        .parked_timed
        .iter()
        .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
        .map(|(&k, _)| k)
        .collect();
    let indef_keys: Vec<u64> = s
        .parked_indef
        .iter()
        .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
        .map(|(&k, _)| k)
        .collect();

    let mut woken = Vec::new();
    for key in timed_keys {
        if !all && !woken.is_empty() {
            break;
        }
        if let Some(entry) = s.parked_timed.remove(&key) {
            woken.push(entry.wake);
        }
    }
    for key in indef_keys {
        if !all && !woken.is_empty() {
            break;
        }
        if let Some(entry) = s.parked_indef.remove(&key) {
            woken.push(entry.wake);
        }
    }
    s.active += woken.len();
    for w in &woken {
        w.mark_granted_under_lock();
    }
    drop(s);
    for t in woken {
        t.fire();
    }
}

/// Handle an async waiter future holds for the lifetime of one park. Carries the
/// engine key so a cancelled (dropped-before-resolve) future can remove its
/// still-parked entry, and the `granted` flag the firer sets so the future can
/// tell a real wake from a cancel and balance `active` exactly.
pub(crate) struct AsyncHandle {
    timed_key: Option<(u64, u64)>,
    indef_key: Option<u64>,
    granted: Arc<AtomicBool>,
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

/// Register a TIMED async sleep waiter `delta_nanos` from the CURRENT virtual
/// instant, then run the advance rule. The deadline is computed from `SIM_NANOS`
/// read INSIDE the lock (so no advance can slip between reading the clock and
/// inserting — a deadline is therefore never below the current clock, the
/// "no backward" invariant). Registration touches no counter: the task is
/// already counted in `active_async` by its poll-wrapper for the current poll,
/// and the waiter is removed by [`cancel_async_wait`] if the future is dropped
/// before it fires.
pub(crate) fn register_sleep_async(delta_nanos: u64, waker: Waker) -> (AsyncHandle, Advance) {
    let granted = Arc::new(AtomicBool::new(false));
    let mut s = SCHED.lock();
    let deadline_nanos = SIM_NANOS
        .load(Ordering::Acquire)
        .saturating_add(delta_nanos);
    let id = s.fresh_id();
    let key = (deadline_nanos, id);
    s.parked_timed.insert(
        key,
        Entry {
            wake: Wake::Task {
                waker,
                granted: Arc::clone(&granted),
            },
            kind: WaitKind::Timed,
        },
    );
    let adv = try_advance_locked(&mut s);
    drop(s);
    (
        AsyncHandle {
            timed_key: Some(key),
            indef_key: None,
            granted,
        },
        adv,
    )
}

/// Register an UNTIMED async `Notify` waiter for `cvid`, OR consume a stored
/// permit. Returns `(handle, to_wake)`; `handle` is `None` when a permit was
/// waiting (the caller resolves at once without parking).
pub(crate) fn register_notify_async(cvid: u64, waker: Waker) -> (Option<AsyncHandle>, Advance) {
    let granted = Arc::new(AtomicBool::new(false));
    let mut s = SCHED.lock();
    if s.notify_permit.remove(&cvid) {
        return (None, Advance { wakes: Vec::new() });
    }
    let id = s.fresh_id();
    s.parked_indef.insert(
        id,
        Entry {
            wake: Wake::Task {
                waker,
                granted: Arc::clone(&granted),
            },
            kind: WaitKind::Condvar(cvid),
        },
    );
    let adv = try_advance_locked(&mut s);
    drop(s);
    (
        Some(AsyncHandle {
            timed_key: None,
            indef_key: Some(id),
            granted,
        }),
        adv,
    )
}

/// Acquire an `active_async` slot for a RUNNABLE async task queued to be polled
/// (just spawned). Adding a participant can never enable an advance, so this does
/// NOT run the advance rule. Called once by [`participate`](crate::flash::participate) at
/// construction; the PARKED→RUNNABLE wake re-acquire goes through
/// [`gate_wake_parked`], which couples the acquire to the state CAS under the lock.
pub(crate) fn async_acquire() {
    SCHED.lock().active_async += 1;
}

/// Decrement the async slot count under a held `SCHED` and return the advance the
/// quiescent edge may unblock. The caller fires it after releasing the lock.
fn release_async_locked(s: &mut SimSched) -> Advance {
    debug_assert!(
        s.active_async > 0,
        "async release without a matching acquire"
    );
    s.active_async -= 1;
    try_advance_locked(s)
}

/// Gate park under the `SCHED` lock: atomically CAS `running`→`parked` and release
/// the async slot, or — a wake landed mid-poll, so the state is `RUNNING_NOTIFIED`
/// and the CAS fails — store `runnable`, keeping the slot for the re-poll the wake
/// already scheduled. Holding the lock across the CAS and the counter update is
/// what closes the wake→poll race: a concurrent [`gate_wake_parked`] acquire can no
/// longer interleave between a lock-free CAS and a separately-locked release, which
/// would leave a re-runnable task uncounted and over-release on its next park.
pub(crate) fn gate_park(state: &AtomicU8, running: u8, parked: u8, runnable: u8) {
    let mut s = SCHED.lock();
    if state
        .compare_exchange(running, parked, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let adv = release_async_locked(&mut s);
        drop(s);
        fire_advance(adv);
    } else {
        state.store(runnable, Ordering::Release);
    }
}

/// Gate completion under the `SCHED` lock: mark `done` and release the slot
/// atomically (mirrors [`gate_park`]'s release arm for a poll that returned Ready).
pub(crate) fn gate_complete(state: &AtomicU8, done: u8) {
    let mut s = SCHED.lock();
    state.store(done, Ordering::Release);
    let adv = release_async_locked(&mut s);
    drop(s);
    fire_advance(adv);
}

/// Gate drop under the `SCHED` lock: swap to `done`; release the slot iff the task
/// still held one (its prior state was counted — `RUNNABLE`/`RUNNING`/`RUNNING_NOTIFIED`).
pub(crate) fn gate_drop_release(
    state: &AtomicU8,
    done: u8,
    runnable: u8,
    running: u8,
    notified: u8,
) {
    let mut s = SCHED.lock();
    let prev = state.swap(done, Ordering::AcqRel);
    if prev == runnable || prev == running || prev == notified {
        let adv = release_async_locked(&mut s);
        drop(s);
        fire_advance(adv);
    }
}

/// Wake a PARKED gate under the `SCHED` lock: CAS `parked`→`runnable` and acquire
/// the slot atomically. Returns `true` when it transitioned (the caller then
/// forwards the runtime waker), `false` if the state was not PARKED (the caller's
/// wake loop re-reads and handles the current state lock-free). Coupling the
/// acquire to the CAS under the lock is the counterpart to [`gate_park`]'s release.
pub(crate) fn gate_wake_parked(state: &AtomicU8, parked: u8, runnable: u8) -> bool {
    let mut s = SCHED.lock();
    if state
        .compare_exchange(parked, runnable, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        s.active_async += 1;
        true
    } else {
        false
    }
}

/// Drop path for an async waiter future cancelled before it resolved (e.g. it
/// lost a `tokio::select!`). Just remove its still-parked entry, if any. Async
/// waiters never hold a counter slot (the firer does not bump `active_async` —
/// the poll-wrapper owns that count per-poll), so there is nothing to release:
/// the surrounding task stays counted by its poll-wrapper either way.
pub(crate) fn cancel_async_wait(handle: &AsyncHandle) {
    let mut s = SCHED.lock();
    match (handle.timed_key, handle.indef_key) {
        (Some(key), _) => {
            s.parked_timed.remove(&key);
        }
        (_, Some(id)) => {
            s.parked_indef.remove(&id);
        }
        _ => {}
    }
}

/// Signal an async `Notify` for `cvid`: wake at most one waiter; if none are
/// parked, store a permit so the next `notified()` resolves at once (tokio
/// `notify_one` semantics).
pub(crate) fn signal_notify(cvid: u64) {
    let mut s = SCHED.lock();
    let woken_key = s
        .parked_indef
        .iter()
        .find(|(_, e)| e.kind == WaitKind::Condvar(cvid))
        .map(|(&k, _)| k);
    let mut woken = Vec::new();
    if let Some(key) = woken_key
        && let Some(entry) = s.parked_indef.remove(&key)
    {
        woken.push(entry.wake);
    }
    if woken.is_empty() {
        // No waiter: store a permit (notify_one) so the next notified() returns
        // immediately.
        s.notify_permit.insert(cvid);
    } else {
        // Async (Task) wakes are counted by the spawn poll-wrapper, not here.
        s.active += woken.iter().filter(|w| !w.is_task()).count();
        for w in &woken {
            w.mark_granted_under_lock();
        }
    }
    drop(s);
    for t in woken {
        t.fire();
    }
}

/// Register an UNTIMED async channel waiter for `cvid`. Unlike
/// [`register_notify_async`] this NEVER consumes a permit: a sim channel
/// (`tokio::sync::mpsc`/`oneshot`) holds its own queue as the source of truth, so
/// the engine waiter is a pure wakeup with no permit bookkeeping. The caller must
/// register WHILE holding the channel's own queue lock so a concurrent producer
/// cannot signal-with-no-waiter between the empty-check and the park (no lost
/// wakeup). Returns `(handle, advance)`; fire the advance after dropping the
/// queue lock.
pub(crate) fn register_channel_async(cvid: u64, waker: Waker) -> (AsyncHandle, Advance) {
    let granted = Arc::new(AtomicBool::new(false));
    let mut s = SCHED.lock();
    let id = s.fresh_id();
    s.parked_indef.insert(
        id,
        Entry {
            wake: Wake::Task {
                waker,
                granted: Arc::clone(&granted),
            },
            kind: WaitKind::Condvar(cvid),
        },
    );
    let adv = try_advance_locked(&mut s);
    drop(s);
    (
        AsyncHandle {
            timed_key: None,
            indef_key: Some(id),
            granted,
        },
        adv,
    )
}

/// Signal a sim channel `cvid`: wake one (`all == false`) or every
/// (`all == true`) parked waiter. Unlike [`signal_notify`] it stores NO permit
/// when none is parked — the channel's queue already holds the produced item (or
/// the closed flag), and the next receiver/sender poll re-checks that queue (or
/// the live-count) directly, so a missed signal is harmless. `all` is for the
/// close edges (a dropped receiver wakes every blocked sender; a dropped last
/// sender wakes the receiver).
pub(crate) fn signal_channel(cvid: u64, all: bool) {
    let mut s = SCHED.lock();
    let keys: Vec<u64> = s
        .parked_indef
        .iter()
        .filter(|(_, e)| e.kind == WaitKind::Condvar(cvid))
        .map(|(&k, _)| k)
        .collect();
    let mut woken = Vec::new();
    for key in keys {
        if !all && !woken.is_empty() {
            break;
        }
        if let Some(entry) = s.parked_indef.remove(&key) {
            woken.push(entry.wake);
        }
    }
    if woken.is_empty() {
        return;
    }
    // Async (Task) wakes are counted by the spawn poll-wrapper, not here.
    s.active += woken.iter().filter(|w| !w.is_task()).count();
    for w in &woken {
        w.mark_granted_under_lock();
    }
    drop(s);
    for w in woken {
        w.fire();
    }
}

/// Clear all engine state (for the in-process test harness; nextest gives
/// production tests per-process isolation).
pub(crate) fn reset() {
    *SCHED.lock() = SimSched::new();
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
/// `register_participant`.
#[cfg(test)]
#[must_use]
pub(crate) struct TestHold {
    _priv: (),
}

#[cfg(test)]
pub(crate) fn test_hold() -> TestHold {
    SCHED.lock().active += 1;
    TestHold { _priv: () }
}

#[cfg(test)]
impl Drop for TestHold {
    fn drop(&mut self) {
        let mut s = SCHED.lock();
        debug_assert!(s.active > 0, "TestHold drop without matching hold");
        s.active -= 1;
        let adv = try_advance_locked(&mut s);
        drop(s);
        fire_advance(adv);
    }
}

#[cfg(test)]
pub(crate) fn advance_log() -> Vec<u64> {
    SCHED.lock().advance_log.clone()
}

/// Test-only: number of currently RUNNING participants.
#[cfg(test)]
pub(crate) fn active_count() -> usize {
    SCHED.lock().active
}

/// Test-only: number of async tasks the engine currently counts as
/// non-quiescent (runnable or running). A task woken but not yet re-polled MUST
/// be counted here, or the clock can advance past it.
#[cfg(test)]
pub(crate) fn async_active_count() -> usize {
    SCHED.lock().active_async
}

/// Test-only: number of currently parked timed waiters.
#[cfg(test)]
pub(crate) fn timed_count() -> usize {
    SCHED.lock().parked_timed.len()
}

/// Test-only: number of currently parked untimed waiters.
#[cfg(test)]
pub(crate) fn indef_count() -> usize {
    SCHED.lock().parked_indef.len()
}

/// Test-only: number of currently parked cooperative-yield waiters.
#[cfg(test)]
pub(crate) fn diag_yield_count() -> usize {
    SCHED.lock().yield_waiters.len()
}

/// Diagnostic snapshot of the engine for hang dumps: counters plus every
/// parked waiter with its kind and deadline relative to the virtual clock.
/// Uses `try_lock` so a dump from a panic/abort path can never itself hang
/// on a held `SCHED` lock.
pub(crate) fn dump() -> String {
    use std::fmt::Write as _;
    let now = SIM_NANOS.load(Ordering::Acquire);
    let Some(s) = SCHED.try_lock() else {
        return format!("virtual_now_ns={now}; SCHED lock held — engine mid-operation");
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "virtual_now_ns={now} active={} active_async={} real_io={} pace_anchor={} yield_waiters={}",
        s.active,
        s.active_async,
        s.real_io,
        if s.pace_anchor.is_some() {
            "set"
        } else {
            "none"
        },
        s.yield_waiters.len(),
    );
    for ((deadline, id), entry) in &s.parked_timed {
        let _ = writeln!(
            out,
            "  timed id={id} kind={:?} deadline_in_ns={}",
            entry.kind,
            deadline.saturating_sub(now),
        );
    }
    for (id, entry) in &s.parked_indef {
        let _ = writeln!(out, "  indef id={id} kind={:?}", entry.kind);
    }
    if !s.unpark_pending.is_empty() {
        let _ = writeln!(out, "  unpark_pending={:?}", s.unpark_pending);
    }
    if !s.notify_permit.is_empty() {
        let _ = writeln!(out, "  notify_permit={:?}", s.notify_permit);
    }
    out
}
