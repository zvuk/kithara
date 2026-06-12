//! The flash engine's ONE per-thread context. Every thread-local the engine
//! consults lives in the single [`ThreadCtx`] cell: the two-flag time mode
//! (`ambient` / `active`), the quiescence credit, the async-poll depth and the
//! dedicated-pacer flag. Fields are private — the mode is written only by the
//! RAII scopes in `api.rs` (via [`push_ambient`] / [`push_active`] /
//! [`restore_mode`]); credit / poll-depth / dedicated only by the credit
//! functions in `system/credit.rs`, each touching ONLY its own field (a
//! pooled thread's credit reset must not clobber the mode of the scope
//! around it).
//!
//! # LIFO premise (whole-`Mode` save/restore)
//!
//! A mode scope saves and restores the WHOLE [`Mode`] (`Copy`, cheap), not
//! just the flag it sets. That is equivalent to a per-field restore ONLY
//! under strictly LIFO nesting of mode scopes on a thread — which every
//! current holder satisfies: the per-poll wrapper guards
//! (`WithAmbient`/`FlashDynamic`), the spawn-bracket scopes, the prod
//! `#[kithara::flash]` guard and the test macro's ambient scope. A non-LIFO
//! holder (a scope outliving a sibling created after it) would silently
//! restore a stale flag — e.g. drag `ambient = true` into foreign code,
//! re-latching stateful primitives constructed there. Do not add one.

use std::cell::Cell;

/// Two-flag flash mode of the current thread. The STATELESS time primitives
/// (`Instant::now`, `thread::park_timeout`, `sleep`/`yield_now`/`unpark`,
/// `sleep`/`timeout`) consult `active` via [`flash_enabled`]; the STATEFUL
/// sync primitives (Condvar/Notify/mpsc/oneshot) latch `ambient` via
/// [`flash_ambient`] once at construction. Default = REAL (both false).
#[derive(Clone, Copy)]
pub(in crate::flash) struct Mode {
    /// Per-test gate: "is this test flash-eligible?" Set by the test macro,
    /// propagated across spawn. A gate — only [`push_active`] consults it to
    /// decide whether a prod flash region may take effect.
    ambient: bool,
    /// Dynamic: "is flash propagating on this callstack right now?" Pushed by
    /// a prod `#[kithara::flash(true)]` guard (only when ambient).
    active: bool,
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
    mode: Cell<Mode>,
    credit: Cell<Credit>,
    /// Nesting depth of an async-task poll on THIS OS thread. Non-zero means
    /// a runtime worker is currently inside
    /// [`Participating::poll`](crate::flash::Participating) — driving a task
    /// that occupies an `active_async` slot. See
    /// `system/credit.rs::in_async_poll` for the bridged-wait contract.
    poll_depth: Cell<u32>,
    /// True iff this OS thread is a DEDICATED participant (a `spawn_named`
    /// pacer or an ambient `spawn_blocking` closure). See
    /// `system/credit.rs::mark_dedicated` for the pacer contract.
    dedicated: Cell<bool>,
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

/// Push a dynamic flash mode: `active = on && ambient` (a prod flash region
/// takes effect only under the ambient gate; `on = false` always carves
/// REAL). Returns the previous whole [`Mode`] for the scope's drop. Writer:
/// the `FlashScope` RAII scope only.
pub(in crate::flash) fn push_active(on: bool) -> Mode {
    CTX.with(|c| {
        let prev = c.mode.get();
        c.mode.set(Mode {
            active: on && prev.ambient,
            ..prev
        });
        prev
    })
}

/// Push the per-test ambient gate. Returns the previous whole [`Mode`] for
/// the scope's drop. Writer: the `AmbientScope` RAII scope only.
pub(in crate::flash) fn push_ambient(on: bool) -> Mode {
    CTX.with(|c| {
        let prev = c.mode.get();
        c.mode.set(Mode {
            ambient: on,
            ..prev
        });
        prev
    })
}

/// Restore a scope's saved whole [`Mode`] (see the LIFO premise in the
/// module doc). Writer: the mode RAII scopes' `Drop` only.
pub(in crate::flash) fn restore_mode(prev: Mode) {
    CTX.with(|c| c.mode.set(prev));
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
