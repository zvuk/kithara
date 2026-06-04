use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::Ordering},
};

use parking_lot::{Condvar, Mutex};

use super::SIM_NANOS;

/// One waiter's wake handle: a flag + condvar so the parked OS thread blocks
/// off-lock and wakes when the engine (clock jump), an `unpark`, or a
/// `signal_condvar` fires it. The flag is set under its own lock before
/// `notify_all`, so a fire that lands before the block is not lost.
pub(crate) struct Token {
    woken: Mutex<bool>,
    cv: Condvar,
}

impl Token {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            woken: Mutex::new(false),
            cv: Condvar::new(),
        })
    }
    pub(crate) fn fire(&self) {
        {
            let mut g = self.woken.lock();
            *g = true;
        }
        self.cv.notify_all();
    }
    pub(crate) fn wait(&self) {
        let mut g = self.woken.lock();
        while !*g {
            self.cv.wait(&mut g);
        }
    }
}

/// What kind of waiter an [`Entry`] is, so a signal targets the right group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitKind {
    /// A timed park with no early-wake channel (used only by the test harness
    /// `park_for`): woken solely by the engine crossing its deadline.
    #[cfg(test)]
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
    token: Arc<Token>,
    kind: WaitKind,
}

/// Process-global quiescence scheduler. ALL fields mutate only under this lock.
/// `SIM_NANOS` is written only by `try_advance_locked` (and the test-only
/// additive `advance`, which the harness never calls).
struct SimSched {
    /// Participants currently RUNNING (not inside a wrapped wait).
    active: usize,
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
            parked_timed: BTreeMap::new(),
            parked_indef: BTreeMap::new(),
            unpark_pending: BTreeSet::new(),
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

static SCHED: Mutex<SimSched> = Mutex::new(SimSched::new());

/// Per-thread quiescence credit. Participants are NOT registered explicitly:
/// a thread is invisible to the engine until its FIRST wrapped wait, at which
/// point it credits itself lazily. This keeps accounting intrinsic to the
/// platform-wrapped primitives — no consumer ever calls a registration API.
///
/// - `None`: the thread has never entered a wrapped wait. It is uncounted: it
///   cannot stall the engine (it owns no deadline and is not in `active`).
/// - `Running`: the thread is currently counted in `s.active` (it woke from a
///   wait, or its first wait already bootstrapped it). The engine will not
///   advance the clock while any thread is `Running`.
/// - `Parked`: the thread is inside a wrapped wait, not counted in `active`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Credit {
    None,
    Running,
    Parked,
}

thread_local! {
    static CREDIT: Cell<Credit> = const { Cell::new(Credit::None) };
}

/// Account this thread as it ENTERS a wrapped wait, under the `SCHED` lock.
/// Called at the start of EACH wrapped wait (park/condvar) right where the
/// entry is inserted, replacing the old explicit `active -= 1`.
///
/// - `None` (first ever wait): the thread was running uncounted. Bootstrap it
///   by transitioning to `Parked` WITHOUT decrementing `active` — it was never
///   added, so there is nothing to remove. Its eventual wake will `active += 1`
///   (by the firer) and `mark_running` it, balancing the books.
/// - `Running` (woke from a prior wait, now parking again): it IS counted, so
///   `active -= 1` and move to `Parked`.
/// - `Parked`: unreachable — a thread waits on one thing at a time.
fn enter_wait_locked(s: &mut SimSched) {
    CREDIT.with(|c| match c.get() {
        Credit::None => c.set(Credit::Parked),
        Credit::Running => {
            debug_assert!(
                s.active > 0,
                "running participant must be counted in active"
            );
            s.active -= 1;
            c.set(Credit::Parked);
        }
        Credit::Parked => debug_assert!(false, "a thread cannot enter two wrapped waits at once"),
    });
}

/// Mark this thread RUNNING after its wrapped wait returned. The firer already
/// did `active += 1` for the woken entry; the woken thread only updates its own
/// credit here.
fn mark_running() {
    CREDIT.with(|c| c.set(Credit::Running));
}

/// Decrement `active` for a thread that EXITS while `Running` — the balancing
/// half of the bootstrap (`None -> Parked` left `active` untouched; the first
/// wake then `active += 1`'d it). Called from the spawn bracket after the
/// thread's body returns. Reads + clears the credit; if it was `Running`, drops
/// it from `active` and fires any advance the drop unblocks.
pub(crate) fn on_participant_exit() {
    let was = CREDIT.with(|c| {
        let v = c.get();
        c.set(Credit::None);
        v
    });
    if was != Credit::Running {
        return;
    }
    let mut s = SCHED.lock();
    debug_assert!(s.active > 0, "exiting running participant must be counted");
    s.active -= 1;
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    for t in to_wake {
        t.fire();
    }
}

/// Reset this thread's credit to `None`. Called at the start of a pooled
/// thread's body (spawn bracket) so a reused OS thread does not inherit a stale
/// credit from a previous task.
pub(crate) fn reset_credit() {
    CREDIT.with(|c| c.set(Credit::None));
}

/// Evaluate the advance rule while holding `SCHED`. Returns the tokens to fire
/// AFTER the caller releases the lock (firing under the lock would make the
/// woken thread immediately contend on SCHED). Operates only on `parked_timed`
/// — it never fires `parked_indef`, which has no deadline. Fires nothing unless
/// every participant is parked (`active == 0`) and at least one timed waiter
/// exists.
fn try_advance_locked(s: &mut SimSched) -> Vec<Arc<Token>> {
    if s.active != 0 {
        return Vec::new();
    }
    let Some((&(min, _), _)) = s.parked_timed.iter().next() else {
        // active == 0 and no timed waiter: nothing to advance to. (Signal-only
        // quiescence detection belongs to a later increment + the watchdog.)
        return Vec::new();
    };
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
            woken.push(entry.token);
        }
    }
    // Pre-increment for the waiters we are about to wake, so a woken thread does
    // NOT re-increment `active` on return — it just resumes running.
    s.active += woken.len();
    woken
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
pub(crate) fn park_for(d: super::Duration) {
    let delta = super::duration_to_nanos(d);
    let token = Token::new();
    let mut s = SCHED.lock();
    let deadline = SIM_NANOS.load(Ordering::Acquire).saturating_add(delta);
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline, id),
        Entry {
            token: Arc::clone(&token),
            kind: WaitKind::Timed,
        },
    );
    enter_wait_locked(&mut s);
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    for t in to_wake {
        t.fire();
    }
    token.wait();
    mark_running();
}

/// Unparkable thread park: block until the virtual clock reaches `now + d` OR
/// [`unpark`] targets `thread_id`. Computes the deadline and registers the
/// entry + `active -= 1` in ONE `SCHED` hold, so no advance can slip between
/// reading the clock and inserting. A pending `unpark` (one that arrived while
/// this thread was running) is consumed here and returns immediately without
/// parking or touching `active`.
pub(crate) fn park_timed_unparkable(d: super::Duration, thread_id: u64) {
    let delta = super::duration_to_nanos(d);
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
            token: Arc::clone(&token),
            kind: WaitKind::Thread(thread_id),
        },
    );
    enter_wait_locked(&mut s);
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    for t in to_wake {
        t.fire();
    }
    token.wait();
    mark_running();
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
        entry.token.fire();
        return;
    }
    s.unpark_pending.insert(thread_id);
}

/// Register a TIMED condvar waiter (woken by the deadline OR a signal for
/// `cvid`). The caller holds the DOMAIN guard when calling this (lock order
/// domain -> SCHED). Accounts the wait via [`enter_wait_locked`], evaluates the
/// advance rule, and returns the token to block on plus the advance-due tokens
/// the caller must fire AFTER releasing the domain guard. The caller calls
/// [`mark_running_after_condvar`] once `token.wait()` returns.
pub(crate) fn register_condvar_timed(
    deadline_nanos: u64,
    cvid: u64,
) -> (Arc<Token>, Vec<Arc<Token>>) {
    let token = Token::new();
    let mut s = SCHED.lock();
    let id = s.fresh_id();
    s.parked_timed.insert(
        (deadline_nanos, id),
        Entry {
            token: Arc::clone(&token),
            kind: WaitKind::Condvar(cvid),
        },
    );
    enter_wait_locked(&mut s);
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    (token, to_wake)
}

/// Register an UNTIMED condvar waiter (no deadline; woken only by a signal for
/// `cvid`). Same return shape and accounting as [`register_condvar_timed`].
pub(crate) fn register_condvar_untimed(cvid: u64) -> (Arc<Token>, Vec<Arc<Token>>) {
    let token = Token::new();
    let mut s = SCHED.lock();
    let id = s.fresh_id();
    s.parked_indef.insert(
        id,
        Entry {
            token: Arc::clone(&token),
            kind: WaitKind::Condvar(cvid),
        },
    );
    enter_wait_locked(&mut s);
    let to_wake = try_advance_locked(&mut s);
    drop(s);
    (token, to_wake)
}

/// Mark the calling thread RUNNING after a condvar wait's `token.wait()` has
/// returned. The condvar wrapper blocks off-lock (inside `MutexGuard::unlocked`)
/// so it cannot call the private `mark_running`; this is the crate-internal
/// hook it uses instead. The firer already `active += 1`'d the woken entry.
pub(crate) fn mark_running_after_condvar() {
    mark_running();
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
            woken.push(entry.token);
        }
    }
    for key in indef_keys {
        if !all && !woken.is_empty() {
            break;
        }
        if let Some(entry) = s.parked_indef.remove(&key) {
            woken.push(entry.token);
        }
    }
    s.active += woken.len();
    drop(s);
    for t in woken {
        t.fire();
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
        let to_wake = try_advance_locked(&mut s);
        drop(s);
        for t in to_wake {
            t.fire();
        }
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
