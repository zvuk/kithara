use std::{marker::PhantomData, mem, panic::Location, sync::atomic::Ordering};

use super::{Core, FLASH, FlashInner, SyncHolder};
use crate::{
    common::thread_id::ACTIVE_NAMED_THREADS,
    flash::{
        ctx::{self, Credit},
        ids::ThreadKey,
    },
    native::thread::current,
};

/// `ThreadKey` of the calling OS thread — the key under which it appears in the
/// engine's sync-holder dump map (matches `park_timeout`'s keying).
fn current_thread_key() -> ThreadKey {
    ThreadKey::of(current().id())
}

/// The calling OS thread's name for the sync-holder dump (`spawn_named` pacers
/// are always named; pool/main threads may be anonymous). Pure introspection —
/// no engine interaction.
fn current_thread_name() -> Option<String> {
    std::thread::current().name().map(str::to_owned)
}

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
    // Name this pacer in the sync-holder dump for as long as it holds its slot
    // `Running` (removed on its first park / on exit). Diagnostic only.
    FLASH.sync_holder_running();
}

/// True iff the current OS thread is a dedicated pacer (see [`mark_dedicated`]).
fn is_dedicated() -> bool {
    ctx::dedicated()
}

/// A DEDICATED pacer's reservation, minted on the PARENT thread BEFORE the
/// child is spawned/queued and moved into the child to be claimed. A pacer
/// runs a warm-up burst (decode the first chunks, fill the ring) before it
/// ever parks; counting it only once the child runs [`mark_dedicated`] leaves
/// a window — between `spawn` returning and the OS scheduling the child — in
/// which a sibling consumer can park, see `active == 0`, and let the virtual
/// clock jump to its watchdog deadline before the pacer has produced
/// anything. Reserving synchronously at spawn closes that window; the claim
/// then only marks the child's credit `Running`.
///
/// The slot OWNS its reservation: dropping it unconsumed (a failed spawn, an
/// unwind before the claim, a closure the pool never ran) returns every
/// reserved resource — the `active` slot, and for the `spawn_named` variant
/// the named-thread count — instead of wedging the engine.
#[must_use]
pub(crate) struct DedicatedSlot {
    /// True for the `spawn_named` variant, which owns the
    /// `ACTIVE_NAMED_THREADS` increment alongside the `active` reservation.
    /// The `spawn_blocking` variant owns only the credit.
    named: bool,
}

impl DedicatedSlot {
    /// Claim the reservation on a `spawn_named` child: mark this thread a
    /// dedicated pacer holding the reserved slot as `Running`. The returned
    /// [`Participant`] owes the exit settle (and the named-count decrement).
    pub(crate) fn claim_dedicated(self) -> Participant {
        debug_assert!(self.named, "claim_dedicated on a spawn_blocking slot");
        let named = self.named;
        mem::forget(self);
        mark_dedicated();
        Participant {
            named,
            _not_send: PhantomData,
        }
    }

    /// Claim the reservation on a POOLED blocking thread for ONE closure: an
    /// ambient `task::spawn_blocking` closure is real work in flight, so the
    /// clock must not advance while it runs (its engine parks still release
    /// the credit, exactly like a `spawn_named` pacer). The returned
    /// [`PoolParticipant`] settles the exit and restores the pool thread's
    /// previous dedicated flag, so a reused thread does not pace later
    /// unrelated tasks.
    pub(crate) fn claim_pooled(self) -> PoolParticipant {
        debug_assert!(!self.named, "claim_pooled on a spawn_named slot");
        mem::forget(self);
        let prev = ctx::dedicated();
        mark_dedicated();
        PoolParticipant {
            prev_dedicated: prev,
            _not_send: PhantomData,
        }
    }

    /// Reserve for an ambient `spawn_blocking` closure: the `active` slot
    /// only (the named-thread count is not this path's resource).
    pub(crate) fn reserve() -> Self {
        FLASH.pre_count_dedicated();
        Self { named: false }
    }

    /// Reserve for a `spawn_named` pacer thread: the `active` slot AND the
    /// named-thread count, both returned by Drop if the slot is never claimed.
    pub(crate) fn reserve_named() -> Self {
        ACTIVE_NAMED_THREADS.fetch_add(1, Ordering::Release);
        FLASH.pre_count_dedicated();
        Self { named: true }
    }
}

impl Drop for DedicatedSlot {
    fn drop(&mut self) {
        // Unconsumed reservation: no thread ever claimed the slot as its
        // credit, so return the raw `active` reservation (and the named count)
        // directly — the release may itself be the quiescent edge.
        FLASH.release_unclaimed_slot();
        if self.named {
            ACTIVE_NAMED_THREADS.fetch_sub(1, Ordering::Release);
        }
    }
}

/// RAII exit settle of a dedicated `spawn_named` pacer (and, via
/// [`Participant::unreserved`], of a slot-less non-ambient pool closure):
/// Drop (incl. unwind through a panicking body) runs the participant-exit
/// settle — read + clear the credit, release the `active` slot if the thread
/// exits `Running` — and decrements the named-thread count when this
/// participant owns it.
#[must_use]
pub(crate) struct Participant {
    _not_send: PhantomData<*mut ()>,
    named: bool,
}

impl Participant {
    /// Exit settle WITHOUT a reservation: the non-ambient `spawn_blocking`
    /// arm. Such a closure is invisible to the engine (it never becomes
    /// `Running` through the ambient bracket), so the settle is a defensive
    /// no-op on the happy path — but RAII keeps the exit unwind-safe and
    /// consistent with the ambient arm.
    pub(crate) fn unreserved() -> Self {
        Self {
            named: false,
            _not_send: PhantomData,
        }
    }
}

impl Drop for Participant {
    fn drop(&mut self) {
        FLASH.on_participant_exit();
        if self.named {
            ACTIVE_NAMED_THREADS.fetch_sub(1, Ordering::Release);
        }
    }
}

/// RAII making a POOLED blocking thread a dedicated pacer for ONE closure
/// (see [`DedicatedSlot::claim_pooled`]). Drop settles the credit and
/// restores the pool thread's previous dedicated flag.
#[must_use]
pub(crate) struct PoolParticipant {
    _not_send: PhantomData<*mut ()>,
    prev_dedicated: bool,
}

impl Drop for PoolParticipant {
    fn drop(&mut self) {
        FLASH.on_participant_exit();
        ctx::set_dedicated(self.prev_dedicated);
    }
}

/// RAII bracket marking the current OS thread as inside an async-task poll for the
/// guard's lifetime. Held by [`Participating::poll`](crate::flash::Participating) around
/// the inner future's poll, so a wrapped sync wait taken inside that poll is
/// recognised as bridged (see [`in_async_poll`]). Drop-safe across an unwind.
pub(in crate::flash) struct AsyncPollGuard {
    /// The top-of-stack task identity to restore on drop (LIFO), so a nested
    /// poll on the same thread does not lose the outer task's identity.
    prev_cur: Option<(u64, &'static Location<'static>)>,
    _not_send: PhantomData<*mut ()>,
}

impl AsyncPollGuard {
    /// Enter a task poll: bump the poll depth and publish this task's identity
    /// (`id`, spawn `loc`) as the top of the stack, so a bridged sync wait taken
    /// inside the poll can keep the async-holder dump map exact.
    pub(in crate::flash) fn enter(id: u64, loc: &'static Location<'static>) -> Self {
        ctx::set_poll_depth(ctx::poll_depth().saturating_add(1));
        let prev_cur = ctx::swap_cur_async(Some((id, loc)));
        Self {
            prev_cur,
            _not_send: PhantomData,
        }
    }
}

impl Drop for AsyncPollGuard {
    fn drop(&mut self) {
        ctx::swap_cur_async(self.prev_cur);
        ctx::set_poll_depth(ctx::poll_depth().saturating_sub(1));
    }
}

/// Obligation to settle ONE wrapped engine wait, minted by
/// [`FlashInner::enter_wait_locked`] in the same `core` hold that inserts the
/// entry, consumed exactly once after `token.wait()` returns:
///
/// - [`WaitGuard::resume`] — every production wait (parks, sleeps, yields,
///   condvar brackets): the firer's `active` bump is settled per the thread's
///   credit class.
/// - [`WaitGuard::mark_running`] — the harness `park_for` ONLY (see its
///   asymmetry note).
///
/// Carries the engine instance the wait was entered on, so the settle always
/// lands on the same `Core`. Dropping the guard unconsumed is a broken wait
/// bracket (the firer's bump would leak) — `debug_assert` in Drop.
#[must_use]
pub(crate) struct WaitGuard<'a> {
    flash: &'a FlashInner,
    _not_send: PhantomData<*mut ()>,
}

impl WaitGuard<'_> {
    /// Test-only bare settle: mark this thread `Running`, keeping the firer's
    /// bump, WITHOUT the credit-class dispatch of [`WaitGuard::resume`]. The
    /// harness `park_for` site is un-bracketed (no spawn bracket balances its
    /// credit), so the non-dedicated `resume` arm would wrongly
    /// `active -= 1` + advance there. Deliberately asymmetric — do not
    /// convert `park_for` to `resume()`.
    #[cfg(test)]
    pub(super) fn mark_running(self) {
        mem::forget(self);
        mark_running();
    }

    /// Settle the firer's `active` bump per this thread's credit class —
    /// see [`FlashInner::resume_after_wait`].
    pub(crate) fn resume(self) {
        let flash = self.flash;
        mem::forget(self);
        flash.resume_after_wait();
    }
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        // Mid-unwind the guard legitimately drops unconsumed (a panic between
        // mint and settle — e.g. inside `WakeBatch::fire` or `Token::wait`);
        // asserting there would double-panic into an abort and mask the root
        // panic. The assert only polices the non-panicking path.
        debug_assert!(
            std::thread::panicking(),
            "WaitGuard dropped without resume()/mark_running() — wrapped wait left unsettled"
        );
    }
}

/// Mark this thread RUNNING after its wrapped wait returned. The firer already
/// did `active += 1` for the woken entry; the woken thread only updates its own
/// credit here.
fn mark_running() {
    ctx::set_credit(Credit::Running);
}

/// Reset this thread's credit to `None`. Called at the start of a pooled
/// thread's body (spawn bracket) so a reused OS thread does not inherit a stale
/// credit from a previous task.
pub(crate) fn reset_credit() {
    ctx::set_credit(Credit::None);
}

impl FlashInner {
    /// Account this thread as it ENTERS a wrapped wait, under the engine's
    /// `core` lock. Called at the start of EACH wrapped wait (park/condvar)
    /// right where the entry is inserted, replacing the old explicit
    /// `active -= 1`. Returns the [`WaitGuard`] obligation the caller consumes
    /// once `token.wait()` returns.
    ///
    /// - `None` (first ever wait): the thread was running uncounted. Bootstrap it
    ///   by transitioning to `Parked` WITHOUT decrementing `active` — it was never
    ///   added, so there is nothing to remove. Its eventual wake will `active += 1`
    ///   (by the firer) and `mark_running` it, balancing the books.
    /// - `Running` (woke from a prior wait, now parking again): it IS counted, so
    ///   `active -= 1` and move to `Parked`.
    /// - `Parked`: unreachable — a thread waits on one thing at a time.
    pub(super) fn enter_wait_locked(&self, s: &mut Core) -> WaitGuard<'_> {
        // The guard is minted LAST (after the transition's own debug_asserts),
        // so an assertion panic here never double-panics in the guard's Drop.
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
            // Keep the diagnostic holder map exact: this task released its slot
            // for the bridged block, so drop it (re-inserted on resume).
            if let Some((id, _)) = ctx::cur_async() {
                s.registry.active_async_holders.remove(&id);
            }
        } else if !is_dedicated() {
            // Non-dedicated, non-async-poll thread (a tokio worker driving a raw-spawned
            // task, the main/test thread, a raw `thread::spawn`): NOT a virtual-time
            // pacer. Register the wakeup but stay OUT of `active` — entering it leaks
            // (nothing balances the firer's wake bump, which `resume_after_wait` instead
            // undoes). The clock is free to advance while such a thread blocks.
        } else {
            match ctx::credit() {
                Credit::None => ctx::set_credit(Credit::Parked),
                Credit::Running => {
                    debug_assert!(
                        s.registry.active > 0,
                        "running participant must be counted in active"
                    );
                    s.registry.active -= 1;
                    ctx::set_credit(Credit::Parked);
                    // This pacer no longer holds its `active` slot — drop it from
                    // the diagnostic holder map (re-added when it resumes Running).
                    s.registry.active_sync_holders.remove(&current_thread_key());
                }
                Credit::Parked => {
                    debug_assert!(false, "a thread cannot enter two wrapped waits at once");
                }
            }
        }
        WaitGuard {
            flash: self,
            _not_send: PhantomData,
        }
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
        s.registry.active_sync_holders.remove(&current_thread_key());
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
    }

    /// Reserve a dedicated pacer's slot: raw `active += 1` on the parent,
    /// before the child is scheduled (see [`DedicatedSlot`]).
    pub(in crate::flash) fn pre_count_dedicated(&self) {
        self.core.lock().registry.active += 1;
    }

    /// Return a reservation no thread ever claimed ([`DedicatedSlot`] Drop):
    /// undo the raw `active += 1` and fire any advance the release unblocks.
    /// No credit is touched — the slot never became any thread's `Running`.
    fn release_unclaimed_slot(&self) {
        let mut s = self.core.lock();
        debug_assert!(
            s.registry.active > 0,
            "unclaimed slot release without a matching reserve"
        );
        s.registry.active -= 1;
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
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
            // Re-insert the holder dropped when this task entered the bridged
            // wait (keeps the diagnostic map matched to the counter).
            if let Some((id, loc)) = ctx::cur_async() {
                s.registry.active_async_holders.insert(id, loc);
            }
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
        // Pacer is `Running` again — restore it to the diagnostic holder map.
        self.sync_holder_running();
    }

    /// Record (or refresh) the calling dedicated pacer thread as a `Running`
    /// sync `active` holder in the diagnostic dump map. Called when it claims its
    /// slot ([`mark_dedicated`]) and each time it resumes `Running` from a wait
    /// (dedicated [`resume_after_wait`](FlashInner::resume_after_wait)). Keyed by
    /// the thread; named by it. Diagnostic only — it does not touch `active`.
    pub(in crate::flash) fn sync_holder_running(&self) {
        let key = current_thread_key();
        let name = current_thread_name();
        self.core
            .lock()
            .registry
            .active_sync_holders
            .insert(key, SyncHolder { name });
    }
}
