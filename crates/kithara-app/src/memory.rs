//! Debug-build ceiling on live heap bytes.
//!
//! The first allocation that carries the process past the ceiling prints the
//! stack that crossed it and aborts, so an unbounded allocator is caught at
//! its own call site instead of when the machine runs out of memory. A release
//! build installs nothing: `main` registers [`Ceiling`] as the global
//! allocator only under `debug_assertions`, so the counters below never run.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    backtrace::Backtrace,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Live heap a debug build tolerates before it aborts.
pub const DEFAULT_LIMIT_BYTES: usize = 4 << 30;

/// Everything [`Ceiling`] keeps between allocations.
///
/// One value rather than three loose statics: they are read and written
/// together on every allocation, and a reader that finds them apart has to
/// work out for itself that they belong to the same ceiling.
struct Counters {
    /// Live heap bytes.
    live: AtomicUsize,
    /// Ceiling in bytes; `0` lets the process allocate without a bound.
    limit: AtomicUsize,
    /// Set by the allocation that crossed the ceiling, so the reporting path —
    /// which allocates while it formats — does not re-enter itself.
    tripped: AtomicBool,
}

static COUNTERS: Counters = Counters {
    limit: AtomicUsize::new(DEFAULT_LIMIT_BYTES),
    live: AtomicUsize::new(0),
    tripped: AtomicBool::new(false),
};

/// Move the ceiling to `bytes`, or lift it with `0`.
pub fn set_limit(bytes: usize) {
    COUNTERS.limit.store(bytes, Ordering::Relaxed);
}

/// Report the crossing and end the process.
///
/// Written with `eprintln!` rather than `tracing`: the subscriber points at
/// `KITHARA_LOG_FILE`, and a process that is about to abort has to leave its
/// stack in the terminal that ran it.
fn report_and_abort(live: usize, limit: usize, requested: usize) -> ! {
    let backtrace = Backtrace::force_capture();
    eprintln!(
        "kithara: live heap {live} bytes crossed the {limit}-byte ceiling on a \
         {requested}-byte allocation\n{backtrace}"
    );
    std::process::abort()
}

/// Charge `bytes` to the live total and abort when that crosses the ceiling.
fn charge(bytes: usize) {
    let live = COUNTERS.live.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let limit = COUNTERS.limit.load(Ordering::Relaxed);
    if limit == 0 || live <= limit || COUNTERS.tripped.swap(true, Ordering::Relaxed) {
        return;
    }
    report_and_abort(live, limit, bytes);
}

/// Release `bytes` from the live total.
fn release(bytes: usize) {
    COUNTERS.live.fetch_sub(bytes, Ordering::Relaxed);
}

/// The system allocator, counting what it hands out against the ceiling.
pub struct Ceiling;

// SAFETY: every method forwards to `System` with the layout it was given and
// returns exactly what `System` returned; the counters around the call read
// and write atomics only.
unsafe impl GlobalAlloc for Ceiling {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        // SAFETY: `layout` is the caller's, passed on untouched.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        // SAFETY: `layout` is the caller's, passed on untouched.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        release(layout.size());
        // SAFETY: `ptr` came from this allocator under `layout`, which forwarded it to `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if let Some(grown) = new_size.checked_sub(layout.size()) {
            charge(grown);
        } else {
            release(layout.size() - new_size);
        }
        // SAFETY: `ptr` came from this allocator under `layout`, and `new_size` is the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn a_growing_realloc_charges_only_the_difference() {
        let layout = Layout::from_size_align(1_024, 8).expect("test layout is valid");
        // SAFETY: the pointer comes from this allocator and is reallocated and
        // freed with the layout it was handed out under.
        unsafe {
            let ptr = Ceiling.alloc(layout);
            let after_alloc = COUNTERS.live.load(Ordering::Relaxed);
            let grown = Ceiling.realloc(ptr, layout, 4_096);
            assert_eq!(COUNTERS.live.load(Ordering::Relaxed), after_alloc + 3_072);
            let grown_layout = Layout::from_size_align(4_096, 8).expect("test layout is valid");
            Ceiling.dealloc(grown, grown_layout);
            assert_eq!(COUNTERS.live.load(Ordering::Relaxed), after_alloc - 1_024);
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_shrinking_realloc_returns_the_difference() {
        let layout = Layout::from_size_align(4_096, 8).expect("test layout is valid");
        // SAFETY: as above.
        unsafe {
            let ptr = Ceiling.alloc(layout);
            let after_alloc = COUNTERS.live.load(Ordering::Relaxed);
            let shrunk = Ceiling.realloc(ptr, layout, 1_024);
            assert_eq!(COUNTERS.live.load(Ordering::Relaxed), after_alloc - 3_072);
            let shrunk_layout = Layout::from_size_align(1_024, 8).expect("test layout is valid");
            Ceiling.dealloc(shrunk, shrunk_layout);
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_lifted_ceiling_tolerates_a_charge_past_the_default() {
        set_limit(0);
        charge(DEFAULT_LIMIT_BYTES * 2);
        release(DEFAULT_LIMIT_BYTES * 2);
        set_limit(DEFAULT_LIMIT_BYTES);
    }
}
