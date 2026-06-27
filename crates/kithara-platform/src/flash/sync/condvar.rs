use std::panic::Location;

use super::mutex::MutexGuard;
use crate::{
    common::time::Instant as RealInstant,
    flash::{
        Instant,
        diag::PrimKind,
        flash_ambient,
        ids::{Backend, trace_native_from_ambient},
        system,
    },
    native::sync::Condvar as NativeCondvar,
};

/// Condvar under `flash`. Each operation matches on the construction-latched
/// [`Backend`] (see its contract): when the test is flash-eligible, waits
/// register on the quiescence engine (keyed by the latched cvid) and
/// `notify_*` signal that group, so a timed wait collapses the virtual clock;
/// otherwise the wrapped [`NativeCondvar`] drives a true wall-clock wait/wake
/// (the default-real path, and the only path until `#[kithara::flash]`
/// annotations land). The engine cvid and the native condvar share the SAME
/// domain mutex, so the unified predicate state is consistent across both
/// park/wake mechanisms.
pub struct Condvar {
    backend: Backend,
    native: NativeCondvar,
}

impl Condvar {
    #[inline]
    pub fn notify_all(&self) {
        match self.backend {
            Backend::Engine(cvid) => system::signal_condvar(cvid, true),
            Backend::Native => {
                trace_native_from_ambient("condvar", "notify_all");
                self.native.notify_all();
            }
        }
    }

    #[inline]
    pub fn notify_one(&self) {
        match self.backend {
            Backend::Engine(cvid) => system::signal_condvar(cvid, false),
            Backend::Native => {
                trace_native_from_ambient("condvar", "notify_one");
                self.native.notify_one();
            }
        }
    }

    /// Untimed wait. Flash: register an untimed engine waiter (no deadline)
    /// BEFORE releasing the domain guard, so any predicate change + notify
    /// (which must take the domain lock, then signal) is serialized after our
    /// engine entry; block off-lock with the guard released, re-acquire on wake.
    /// Native: `Condvar::wait` atomically releases the domain guard
    /// and parks, re-acquiring on wake — the same lost-wakeup-free handshake on
    /// the real lock.
    #[inline]
    #[must_use]
    pub fn wait<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        match self.backend {
            Backend::Engine(cvid) => {
                let (token, adv, wait) = system::register_condvar_untimed(cvid);
                guard.unlocked(move || {
                    adv.fire();
                    token.wait();
                    wait.resume();
                });
                guard
            }
            Backend::Native => {
                trace_native_from_ambient("condvar", "wait");
                if let Some(m) = guard.meta() {
                    m.released();
                }
                self.native.wait_ref(guard.native_mut());
                if let Some(m) = guard.meta() {
                    m.acquired(guard.site());
                }
                guard
            }
        }
    }

    /// Timed wait. Flash: register a timed engine waiter (deadline = `deadline`'s
    /// absolute virtual nanos) BEFORE releasing the domain guard, fire any
    /// advance-due tokens, then block off-lock until the engine crosses the
    /// deadline OR a `notify_*` signals our cvid. Native: wait out the
    /// remaining wall-clock budget (`deadline` is virtual-`Instant` over the
    /// real monotonic clock when flash is off) via the native deadline form —
    /// `wait_timeout(now + remaining)` is `parking_lot`'s own `wait_for`
    /// expansion — so the real condvar wakes on a `notify_*` or the timeout.
    /// Re-acquire on wake; the caller re-checks its predicate (storage loops).
    /// The engine/native entry is taken under the domain guard so a concurrent
    /// predicate-change + notify cannot land between the caller's predicate
    /// check and our park.
    #[inline]
    #[must_use]
    pub fn wait_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        match self.backend {
            Backend::Engine(cvid) => {
                let (token, adv, wait) =
                    system::register_condvar_timed(deadline.as_virtual_nanos(), cvid);
                guard.unlocked(move || {
                    adv.fire();
                    token.wait();
                    wait.resume();
                });
                guard
            }
            Backend::Native => {
                trace_native_from_ambient("condvar", "wait_timeout");
                if let Some(m) = guard.meta() {
                    m.released();
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                self.native
                    .wait_timeout_ref(guard.native_mut(), RealInstant::now() + remaining);
                if let Some(m) = guard.meta() {
                    m.acquired(guard.site());
                }
                guard
            }
        }
    }
}

impl Default for Condvar {
    #[track_caller]
    fn default() -> Self {
        Self {
            backend: if flash_ambient() {
                let cvid = system::next_condvar_id();
                system::describe_cvid(cvid, PrimKind::Condvar, Location::caller());
                Backend::Engine(cvid)
            } else {
                Backend::Native
            },
            native: NativeCondvar::default(),
        }
    }
}
