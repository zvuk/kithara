//! Participant credit accounting for the flash engine: which OS threads count
//! as virtual-time pacers, and how a wrapped wait moves them in and out of the
//! scheduler's `active` count.

use std::cell::Cell;

use super::sched::{self, SimSched};

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
    /// Nesting depth of an async-task poll on THIS OS thread. Non-zero means a
    /// runtime worker is currently inside [`Participating::poll`](super::Participating)
    /// — driving a task that occupies an `active_async` slot. A wrapped sync wait
    /// taken while this is non-zero is a BRIDGED wait: the worker is not a
    /// dedicated pacer (it returns to the runtime when the poll yields, parking on
    /// the runtime, never on the engine), and the task it drives is parked for the
    /// duration of the block. Such a wait releases the `active_async` slot on enter
    /// and re-acquires it on wake (so the clock can advance while the worker blocks
    /// on the engine), and NEVER enters the sync `active` count (which would leak —
    /// the worker has no matching `enter_wait`/exit to balance it).
    static ASYNC_POLL_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// True when the calling OS thread is inside an async-task poll (a runtime worker
/// driving a [`Participating`](super::Participating) future). See [`ASYNC_POLL_DEPTH`].
fn in_async_poll() -> bool {
    ASYNC_POLL_DEPTH.with(|c| c.get() > 0)
}

thread_local! {
    /// True iff this OS thread is a DEDICATED participant: a `spawn_named` thread
    /// (audio worker, offline render thread, flush hub) that loops on wrapped waits
    /// and does real work between them. ONLY dedicated threads are counted in the
    /// sync `active` pacer count — the clock must not advance while one is mid-work,
    /// so its inter-wait running time pins the clock. Every OTHER thread (a tokio
    /// runtime worker, the main/test thread, a raw `thread::spawn`) is NOT a pacer:
    /// its wrapped waits register a wakeup but stay OUT of `active` (entering it
    /// leaks — such a thread parks on the runtime or exits without the counted
    /// bracket, so nothing balances the credit). Set by the `spawn_named` bracket
    /// via [`mark_dedicated`]; default false.
    static DEDICATED: Cell<bool> = const { Cell::new(false) };
}

/// Mark the current OS thread a DEDICATED virtual-time pacer. Called once by the
/// `spawn_named` bracket so only those threads are counted in `active`. See [`DEDICATED`].
pub(crate) fn mark_dedicated() {
    DEDICATED.with(|c| c.set(true));
    // The pacer's `active` slot is taken EAGERLY by [`pre_count_dedicated`] on the
    // PARENT thread at spawn time (closing the spawn→run gap). Here, on the child,
    // we only claim the credit as `Running` to match that slot — no `active += 1`,
    // or the slot would be double-counted. The first wrapped wait drops it
    // (`Running -> Parked`, `active -= 1`); `on_participant_exit` settles it if the
    // thread returns while `Running`.
    CREDIT.with(|c| c.set(Credit::Running));
}

/// Reserve a DEDICATED pacer's `active` slot on the PARENT thread, BEFORE the
/// child is spawned. A `spawn_named` pacer runs a warm-up burst (decode the first
/// chunks, fill the ring) before it ever parks; counting it only once the child
/// runs [`mark_dedicated`] leaves a window — between `spawn` returning and the OS
/// scheduling the child — in which a sibling consumer can park, see `active == 0`,
/// and let the virtual clock jump to its watchdog deadline before the pacer has
/// produced anything. Reserving the slot synchronously at spawn closes that
/// window; the child's [`mark_dedicated`] then only marks its credit `Running`.
pub(crate) fn pre_count_dedicated() {
    sched::SCHED.lock().active += 1;
}

/// True iff the current OS thread is a dedicated pacer (see [`DEDICATED`]).
fn is_dedicated() -> bool {
    DEDICATED.with(Cell::get)
}

/// RAII making a POOLED blocking thread a dedicated pacer for ONE closure: an
/// ambient `task::spawn_blocking` closure is real work in flight, so the clock
/// must not advance while it runs (its engine parks still release the credit,
/// exactly like a `spawn_named` pacer). The caller reserves the `active` slot
/// via [`pre_count_dedicated`] BEFORE the pool queues the closure; `enter`
/// claims it `Running`. Drop (incl. unwind through a panicking closure) settles
/// the credit and restores the pool thread's previous dedicated flag, so a
/// reused thread does not pace later unrelated tasks.
pub(crate) struct BlockingPacer {
    prev_dedicated: bool,
}

impl BlockingPacer {
    pub(crate) fn enter() -> Self {
        let prev = DEDICATED.with(Cell::get);
        mark_dedicated();
        Self {
            prev_dedicated: prev,
        }
    }
}

impl Drop for BlockingPacer {
    fn drop(&mut self) {
        on_participant_exit();
        DEDICATED.with(|c| c.set(self.prev_dedicated));
    }
}

/// RAII bracket marking the current OS thread as inside an async-task poll for the
/// guard's lifetime. Held by [`Participating::poll`](super::Participating) around
/// the inner future's poll, so a wrapped sync wait taken inside that poll is
/// recognised as bridged (see [`ASYNC_POLL_DEPTH`]). Drop-safe across an unwind.
pub(super) struct AsyncPollGuard {
    _priv: (),
}

impl AsyncPollGuard {
    pub(super) fn enter() -> Self {
        ASYNC_POLL_DEPTH.with(|c| c.set(c.get().saturating_add(1)));
        Self { _priv: () }
    }
}

impl Drop for AsyncPollGuard {
    fn drop(&mut self) {
        ASYNC_POLL_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
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
pub(super) fn enter_wait_locked(s: &mut SimSched) {
    if in_async_poll() {
        // Bridged wait: a runtime worker is blocking on the engine mid async-poll.
        // The task it drives is parked for the block, so release its `active_async`
        // slot (re-acquired by `resume_after_wait` on wake) — otherwise the slot
        // pins the clock and the event that would unblock the wait can never fire.
        debug_assert!(
            s.active_async > 0,
            "bridged wait must be inside a counted async poll"
        );
        s.active_async -= 1;
        return;
    }
    if !is_dedicated() {
        // Non-dedicated, non-async-poll thread (a tokio worker driving a raw-spawned
        // task, the main/test thread, a raw `thread::spawn`): NOT a virtual-time
        // pacer. Register the wakeup but stay OUT of `active` — entering it leaks
        // (nothing balances the firer's wake bump, which `resume_after_wait` instead
        // undoes). The clock is free to advance while such a thread blocks.
        return;
    }
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

/// Resume accounting after a wrapped sync wait's `token.wait()` returned. The firer
/// always `active += 1`'d the woken Sync entry to cover wake latency; how that is
/// settled depends on the thread:
/// - BRIDGED (runtime worker mid async-poll): undo the `active` bump and re-acquire
///   the `active_async` slot released on enter.
/// - NON-DEDICATED, non-async: undo the `active` bump (the thread is not a pacer and
///   never entered `active` on the wait side).
/// - DEDICATED pacer: keep the bump and mark the thread `Running`.
pub(super) fn resume_after_wait() {
    if in_async_poll() {
        let mut s = sched::SCHED.lock();
        debug_assert!(s.active > 0, "bridged resume without a firer active bump");
        s.active -= 1;
        s.active_async += 1;
        drop(s);
        return;
    }
    if !is_dedicated() {
        let mut s = sched::SCHED.lock();
        debug_assert!(s.active > 0, "non-pacer resume without a firer active bump");
        s.active -= 1;
        let adv = sched::try_advance_locked(&mut s);
        drop(s);
        sched::fire_advance(adv);
        return;
    }
    mark_running();
}

/// Mark this thread RUNNING after its wrapped wait returned. The firer already
/// did `active += 1` for the woken entry; the woken thread only updates its own
/// credit here.
pub(super) fn mark_running() {
    CREDIT.with(|c| c.set(Credit::Running));
}

/// Mark the calling thread RUNNING after a condvar wait's `token.wait()` has
/// returned. The condvar wrapper blocks off-lock (inside `MutexGuard::unlocked`)
/// so it cannot call the private `mark_running`; this is the crate-internal
/// hook it uses instead. The firer already `active += 1`'d the woken entry.
pub(crate) fn mark_running_after_condvar() {
    resume_after_wait();
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
    let mut s = sched::SCHED.lock();
    debug_assert!(s.active > 0, "exiting running participant must be counted");
    s.active -= 1;
    let adv = sched::try_advance_locked(&mut s);
    drop(s);
    sched::fire_advance(adv);
}

/// Reset this thread's credit to `None`. Called at the start of a pooled
/// thread's body (spawn bracket) so a reused OS thread does not inherit a stale
/// credit from a previous task.
pub(crate) fn reset_credit() {
    CREDIT.with(|c| c.set(Credit::None));
}
