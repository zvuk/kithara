//! `RealtimeSanitizer` escape hatch for intentionally-blocking regions.
//!
//! Under `--cfg rtsan` the audio worker / RT paths carry
//! `#[sanitize(realtime = "nonblocking")]` (via `#[kithara::rtsan_forbid_blocking]`),
//! so `RTSan` aborts on any `malloc` / `free` / lock / syscall reached from them.
//! A function annotated `#[kithara::rtsan_allow_blocking]` opens a [`permit`]
//! guard for its whole body, suspending those checks via the `__rtsan_disable`
//! / `__rtsan_enable` C entry points exported by the linked sanitizer runtime —
//! used to *inventory* genuinely-unavoidable blocking points one at a time
//! while the surrounding coordination stays checked.
//!
//! The guard is **reentrant**: a thread-local depth counter toggles the runtime
//! only at the outermost permit, so annotating both a caller and a callee (or
//! re-entering through recursion) stays correct regardless of whether the
//! runtime's own `disable` is refcounted. Off `rtsan` the guard is a zero-cost
//! ZST.

#[cfg(rtsan)]
unsafe extern "C" {
    fn __rtsan_disable();
    fn __rtsan_enable();
}

#[cfg(rtsan)]
thread_local! {
    /// Nesting depth of live [`Permit`] guards on this thread.
    static PERMIT_DEPTH: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

/// RAII guard that re-enables `RealtimeSanitizer` checks when the outermost
/// guard on the thread drops. Created by [`permit`]; emitted by
/// `#[kithara::rtsan_allow_blocking]`.
#[cfg(rtsan)]
#[must_use = "the permit only suspends RTSan checks while the guard is alive"]
pub struct Permit;

#[cfg(rtsan)]
impl Drop for Permit {
    fn drop(&mut self) {
        PERMIT_DEPTH.with(|depth| {
            let outer = depth.get().saturating_sub(1);
            depth.set(outer);
            if outer == 0 {
                // SAFETY: paired with the outermost `__rtsan_disable` in
                // `permit`; `__rtsan_enable` is a no-arg C entry point from the
                // linked RTSan runtime.
                unsafe { __rtsan_enable() }
            }
        });
    }
}

/// Suspend `RealtimeSanitizer` blocking-checks until the returned guard drops.
///
/// Reentrant: only the outermost live guard toggles the runtime.
#[cfg(rtsan)]
#[inline]
pub fn permit() -> Permit {
    PERMIT_DEPTH.with(|depth| {
        let prev = depth.get();
        depth.set(prev + 1);
        if prev == 0 {
            // SAFETY: `__rtsan_disable` is a no-arg C entry point from the
            // linked RTSan runtime; `Permit::drop` re-enables at depth 0
            // (panic-safe).
            unsafe { __rtsan_disable() }
        }
    });
    Permit
}

/// Zero-cost no-op guard when `RealtimeSanitizer` is not compiled in.
#[cfg(not(rtsan))]
#[must_use]
pub struct Permit;

/// No-op when `RealtimeSanitizer` is not compiled in.
#[cfg(not(rtsan))]
#[inline(always)]
pub fn permit() -> Permit {
    Permit
}
