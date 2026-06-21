use std::{cell::Cell, panic::Location};

/// Two-flag flash mode of the current thread. The STATELESS time primitives
/// (`Instant::now`, `thread::park_timeout`, `sleep`/`yield_now`/`unpark`,
/// `sleep`/`timeout`) consult `active` via [`flash_enabled`]; the STATEFUL
/// sync primitives (Condvar/Notify/mpsc/oneshot) latch `ambient` via
/// [`flash_ambient`] once at construction. Default = REAL (both false).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::flash) struct Mode {
    /// Dynamic: "is flash propagating on this callstack right now?" Pushed by
    /// a prod `#[kithara::flash(true)]` guard (only when ambient).
    active: bool,
    /// Per-test gate: "is this test flash-eligible?" Set by the test macro,
    /// propagated across spawn. A gate — only [`push_active`] consults it to
    /// decide whether a prod flash region may take effect.
    ambient: bool,
}

/// Per-thread quiescence credit. Participants are NOT registered explicitly:
/// a thread is invisible to the engine until its FIRST wrapped wait, at which
/// point it credits itself lazily. This keeps accounting intrinsic to the
/// platform-wrapped primitives — no consumer ever calls a registration API.
///
/// - `None`: the thread has never entered a wrapped wait. It is uncounted: it
///   cannot stall the engine (it owns no deadline and is not in `active`).
/// - `Running`: the thread is currently counted in the engine's `active` (it
///   woke from a wait, or its first wait already bootstrapped it). The engine
///   will not advance the clock while any thread is `Running`.
/// - `Parked`: the thread is inside a wrapped wait, not counted in `active`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::flash) enum Credit {
    None,
    Running,
    Parked,
}

/// The single flash thread-local (see the module doc).
struct ThreadCtx {
    credit: Cell<Credit>,
    /// Identity (`active_async` slot id + spawn site) of the task whose poll is
    /// currently on top of this thread's poll stack — set by
    /// [`Participating::poll`](crate::flash::Participating) around the inner
    /// poll, saved/restored LIFO. Lets a BRIDGED sync wait taken mid-poll keep
    /// the diagnostic async-holder map exact: the wait releases the task's slot
    /// (`active_async -= 1`), so the holder must drop too, and be re-inserted on
    /// resume. Diagnostic only.
    cur_async: Cell<Option<(u64, &'static Location<'static>)>>,
    /// True iff this OS thread is a DEDICATED participant (a `spawn_named`
    /// pacer or an ambient `spawn_blocking` closure). See
    /// `system/credit.rs::mark_dedicated` for the pacer contract.
    dedicated: Cell<bool>,
    mode: Cell<Mode>,
    /// Nesting depth of an async-task poll on THIS OS thread. Non-zero means
    /// a runtime worker is currently inside
    /// [`Participating::poll`](crate::flash::Participating) — driving a task
    /// that occupies an `active_async` slot. See
    /// `system/credit.rs::in_async_poll` for the bridged-wait contract.
    poll_depth: Cell<u32>,
}

thread_local! {
    static CTX: ThreadCtx = const {
        ThreadCtx {
            // REAL by default: not flash-eligible, not propagating.
            mode: Cell::new(Mode {
                ambient: false,
                active: false,
            }),
            credit: Cell::new(Credit::None),
            poll_depth: Cell::new(0),
            dedicated: Cell::new(false),
            cur_async: Cell::new(None),
        }
    };
}

/// True when flash (virtual clock) governs this callstack. Default false
/// (REAL). The per-thread switch the STATELESS time primitives consult:
/// `Instant::now`, `thread::park_timeout`, `thread::sleep`/`yield_now`/
/// `unpark`, `sleep`/`timeout` branch on it.
#[inline]
#[must_use]
pub(crate) fn flash_enabled() -> bool {
    CTX.with(|c| c.mode.get().active)
}

/// True when the current test is flash-eligible (the per-test ambient gate,
/// propagated across spawn). The STATEFUL sync primitives (Condvar/Notify/
/// mpsc/oneshot) branch on THIS — not `flash_enabled()` — so a primitive's
/// wait and its cross-thread signal always agree on real-vs-engine (ambient
/// is uniform per test; `active` is per-callstack and would mismatch across
/// threads).
#[inline]
#[must_use]
pub(crate) fn flash_ambient() -> bool {
    CTX.with(|c| c.mode.get().ambient)
}

/// A mode scope's saved state: the [`Mode`] to restore on drop plus the one
/// the scope itself set — the latter is what makes the LIFO premise checkable
/// (see the module doc).
#[derive(Clone, Copy)]
pub(in crate::flash) struct ModeSnapshot {
    prev: Mode,
    set: Mode,
}

/// Push a dynamic flash mode: `active = on && ambient` (a prod flash region
/// takes effect only under the ambient gate; `on = false` always carves
/// REAL). Returns the scope's [`ModeSnapshot`] for its drop. Writer: the
/// `FlashScope` RAII scope only.
pub(in crate::flash) fn push_active(on: bool) -> ModeSnapshot {
    CTX.with(|c| {
        let prev = c.mode.get();
        let set = Mode {
            active: on && prev.ambient,
            ..prev
        };
        c.mode.set(set);
        ModeSnapshot { prev, set }
    })
}

/// Push the per-test ambient gate. Returns the scope's [`ModeSnapshot`] for
/// its drop. Writer: the `AmbientScope` RAII scope only.
pub(in crate::flash) fn push_ambient(on: bool) -> ModeSnapshot {
    CTX.with(|c| {
        let prev = c.mode.get();
        let set = Mode {
            ambient: on,
            ..prev
        };
        c.mode.set(set);
        ModeSnapshot { prev, set }
    })
}

/// Restore a scope's saved [`Mode`], asserting the LIFO premise (module doc):
/// the current mode must still be the one this scope set — a mismatch means a
/// non-LIFO drop (an interleaved later-created scope is still alive, or was
/// already restored over this one) and the restore would silently resurrect a
/// stale flag. Skipped mid-unwind (a panicking scope stack tears down out of
/// order by design). Writer: the mode RAII scopes' `Drop` only.
pub(in crate::flash) fn restore_mode(snap: ModeSnapshot) {
    CTX.with(|c| {
        debug_assert!(
            c.mode.get() == snap.set || std::thread::panicking(),
            "BUG: non-LIFO mode-scope drop — mode is not the one this scope set (an \
             interleaved later scope is still alive, or was already restored over this one)"
        );
        c.mode.set(snap.prev);
    });
}

/// Read this thread's quiescence credit.
pub(in crate::flash) fn credit() -> Credit {
    CTX.with(|c| c.credit.get())
}

/// Set this thread's quiescence credit (credit functions only; never touches
/// the other fields).
pub(in crate::flash) fn set_credit(v: Credit) {
    CTX.with(|c| c.credit.set(v));
}

/// Read this thread's async-poll nesting depth.
pub(in crate::flash) fn poll_depth() -> u32 {
    CTX.with(|c| c.poll_depth.get())
}

/// Set this thread's async-poll nesting depth (`AsyncPollGuard` only).
pub(in crate::flash) fn set_poll_depth(v: u32) {
    CTX.with(|c| c.poll_depth.set(v));
}

/// Read this thread's dedicated-pacer flag.
pub(in crate::flash) fn dedicated() -> bool {
    CTX.with(|c| c.dedicated.get())
}

/// Set this thread's dedicated-pacer flag (credit functions only).
pub(in crate::flash) fn set_dedicated(v: bool) {
    CTX.with(|c| c.dedicated.set(v));
}

/// Identity of the task whose poll is on top of this thread's poll stack, if
/// any (for the bridged-wait holder bookkeeping — `AsyncPollGuard` only).
pub(in crate::flash) fn cur_async() -> Option<(u64, &'static Location<'static>)> {
    CTX.with(|c| c.cur_async.get())
}

/// Set the top-of-stack task identity, returning the previous value to restore
/// on poll exit (LIFO save/restore — `AsyncPollGuard` only).
pub(in crate::flash) fn swap_cur_async(
    v: Option<(u64, &'static Location<'static>)>,
) -> Option<(u64, &'static Location<'static>)> {
    CTX.with(|c| c.cur_async.replace(v))
}
