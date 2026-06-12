//! Participant credit accounting for the flash engine: which OS threads count
//! as virtual-time pacers, and how a wrapped wait moves them in and out of the
//! engine's `active` count. The per-thread state lives in the single
//! `ThreadCtx` (`flash/ctx.rs`); the engine-state halves are `FlashInner`
//! methods with thin process-engine forwards for the wrappers.

use super::{Core, FLASH, FlashInner};
use crate::flash::ctx::{self, Credit};

/// True when the calling OS thread is inside an async-task poll (a runtime
/// worker driving a [`Participating`](crate::flash::Participating) future) —
/// non-zero poll depth. A wrapped sync wait taken while this holds is a
/// BRIDGED wait: the worker is not a dedicated pacer (it returns to the
/// runtime when the poll yields, parking on the runtime, never on the
/// engine), and the task it drives is parked for the duration of the block.
/// Such a wait releases the `active_async` slot on enter and re-acquires it
/// on wake (so the clock can advance while the worker blocks on the engine),
/// and NEVER enters the sync `active` count (which would leak — the worker
/// has no matching `enter_wait`/exit to balance it).
fn in_async_poll() -> bool {
    ctx::poll_depth() > 0
}

/// Mark the current OS thread a DEDICATED virtual-time pacer: a `spawn_named`
/// thread (audio worker, offline render thread, flush hub) that loops on
/// wrapped waits and does real work between them. ONLY dedicated threads are
/// counted in the sync `active` pacer count — the clock must not advance
/// while one is mid-work, so its inter-wait running time pins the clock.
/// Every OTHER thread (a tokio runtime worker, the main/test thread, a raw
/// `thread::spawn`) is NOT a pacer: its wrapped waits register a wakeup but
/// stay OUT of `active` (entering it leaks — such a thread parks on the
/// runtime or exits without the counted bracket, so nothing balances the
/// credit). Called once by the `spawn_named` bracket.
pub(crate) fn mark_dedicated() {
    ctx::set_dedicated(true);
    // The pacer's `active` slot is taken EAGERLY by [`pre_count_dedicated`] on the
    // PARENT thread at spawn time (closing the spawn→run gap). Here, on the child,
    // we only claim the credit as `Running` to match that slot — no `active += 1`,
    // or the slot would be double-counted. The first wrapped wait drops it
    // (`Running -> Parked`, `active -= 1`); `on_participant_exit` settles it if the
    // thread returns while `Running`.
    ctx::set_credit(Credit::Running);
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
    FLASH.pre_count_dedicated();
}

/// True iff the current OS thread is a dedicated pacer (see [`mark_dedicated`]).
fn is_dedicated() -> bool {
    ctx::dedicated()
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
        let prev = ctx::dedicated();
        mark_dedicated();
        Self {
            prev_dedicated: prev,
        }
    }
}

impl Drop for BlockingPacer {
    fn drop(&mut self) {
        on_participant_exit();
        ctx::set_dedicated(self.prev_dedicated);
    }
}

/// RAII bracket marking the current OS thread as inside an async-task poll for the
/// guard's lifetime. Held by [`Participating::poll`](crate::flash::Participating) around
/// the inner future's poll, so a wrapped sync wait taken inside that poll is
/// recognised as bridged (see [`in_async_poll`]). Drop-safe across an unwind.
pub(in crate::flash) struct AsyncPollGuard {
    _priv: (),
}

impl AsyncPollGuard {
    pub(in crate::flash) fn enter() -> Self {
        ctx::set_poll_depth(ctx::poll_depth().saturating_add(1));
        Self { _priv: () }
    }
}

impl Drop for AsyncPollGuard {
    fn drop(&mut self) {
        ctx::set_poll_depth(ctx::poll_depth().saturating_sub(1));
    }
}

/// Account this thread as it ENTERS a wrapped wait, under the engine's `core`
/// lock. Called at the start of EACH wrapped wait (park/condvar) right where the
/// entry is inserted, replacing the old explicit `active -= 1`.
///
/// - `None` (first ever wait): the thread was running uncounted. Bootstrap it
///   by transitioning to `Parked` WITHOUT decrementing `active` — it was never
///   added, so there is nothing to remove. Its eventual wake will `active += 1`
///   (by the firer) and `mark_running` it, balancing the books.
/// - `Running` (woke from a prior wait, now parking again): it IS counted, so
///   `active -= 1` and move to `Parked`.
/// - `Parked`: unreachable — a thread waits on one thing at a time.
pub(super) fn enter_wait_locked(s: &mut Core) {
    if in_async_poll() {
        // Bridged wait: a runtime worker is blocking on the engine mid async-poll.
        // The task it drives is parked for the block, so release its `active_async`
        // slot (re-acquired by `resume_after_wait` on wake) — otherwise the slot
        // pins the clock and the event that would unblock the wait can never fire.
        debug_assert!(
            s.registry.active_async > 0,
            "bridged wait must be inside a counted async poll"
        );
        s.registry.active_async -= 1;
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
    match ctx::credit() {
        Credit::None => ctx::set_credit(Credit::Parked),
        Credit::Running => {
            debug_assert!(
                s.registry.active > 0,
                "running participant must be counted in active"
            );
            s.registry.active -= 1;
            ctx::set_credit(Credit::Parked);
        }
        Credit::Parked => debug_assert!(false, "a thread cannot enter two wrapped waits at once"),
    }
}

/// Mark this thread RUNNING after its wrapped wait returned. The firer already
/// did `active += 1` for the woken entry; the woken thread only updates its own
/// credit here.
pub(super) fn mark_running() {
    ctx::set_credit(Credit::Running);
}

/// Mark the calling thread RUNNING after a condvar wait's `token.wait()` has
/// returned. The condvar wrapper blocks off-lock (inside `MutexGuard::unlocked`)
/// so it cannot call the private `mark_running`; this is the crate-internal
/// hook it uses instead. The firer already `active += 1`'d the woken entry.
pub(crate) fn mark_running_after_condvar() {
    FLASH.resume_after_wait();
}

/// Process-engine forward of [`FlashInner::on_participant_exit`]. Called from
/// the spawn bracket after the thread's body returns.
pub(crate) fn on_participant_exit() {
    FLASH.on_participant_exit();
}

/// Reset this thread's credit to `None`. Called at the start of a pooled
/// thread's body (spawn bracket) so a reused OS thread does not inherit a stale
/// credit from a previous task.
pub(crate) fn reset_credit() {
    ctx::set_credit(Credit::None);
}

impl FlashInner {
    /// Instance form of [`pre_count_dedicated`]: raw `active += 1` on the
    /// parent, before the child is scheduled.
    pub(in crate::flash) fn pre_count_dedicated(&self) {
        self.core.lock().registry.active += 1;
    }

    /// Resume accounting after a wrapped sync wait's `token.wait()` returned. The firer
    /// always `active += 1`'d the woken Sync entry to cover wake latency; how that is
    /// settled depends on the thread:
    /// - BRIDGED (runtime worker mid async-poll): undo the `active` bump and re-acquire
    ///   the `active_async` slot released on enter.
    /// - NON-DEDICATED, non-async: undo the `active` bump (the thread is not a pacer and
    ///   never entered `active` on the wait side).
    /// - DEDICATED pacer: keep the bump and mark the thread `Running`.
    pub(in crate::flash) fn resume_after_wait(&self) {
        if in_async_poll() {
            let mut s = self.core.lock();
            debug_assert!(
                s.registry.active > 0,
                "bridged resume without a firer active bump"
            );
            s.registry.active -= 1;
            s.registry.active_async += 1;
            drop(s);
            return;
        }
        if !is_dedicated() {
            let mut s = self.core.lock();
            debug_assert!(
                s.registry.active > 0,
                "non-pacer resume without a firer active bump"
            );
            s.registry.active -= 1;
            let adv = s.try_advance(&self.clock);
            drop(s);
            adv.fire();
            return;
        }
        mark_running();
    }

    /// Decrement `active` for a thread that EXITS while `Running` — the balancing
    /// half of the bootstrap (`None -> Parked` left `active` untouched; the first
    /// wake then `active += 1`'d it). Called from the spawn bracket after the
    /// thread's body returns. Reads + clears the credit; if it was `Running`, drops
    /// it from `active` and fires any advance the drop unblocks.
    pub(in crate::flash) fn on_participant_exit(&self) {
        let was = ctx::credit();
        ctx::set_credit(Credit::None);
        if was != Credit::Running {
            return;
        }
        let mut s = self.core.lock();
        debug_assert!(
            s.registry.active > 0,
            "exiting running participant must be counted"
        );
        s.registry.active -= 1;
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
    }
}
