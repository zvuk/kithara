//! Heap measurement for tests: a global allocator that counts what it hands
//! out, so a test can charge a budget to the subsystem that spent it.
//!
//! A test binary installs it once at its root:
//!
//! ```ignore
//! #[global_allocator]
//! static HEAP: kithara_test_utils::memory::Counting = kithara_test_utils::memory::Counting;
//! ```
//!
//! and then reads [`live_bytes`] around the construction it is measuring.
//! [`measure`] wraps that pattern: it returns what the value it built holds,
//! so a suite can name one line per subsystem instead of one total.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicIsize, Ordering},
};

/// Live heap bytes, maintained by [`Counting`].
///
/// Signed, because the counter only sees what this binary allocates: a target
/// that links `std` dynamically frees buffers through here that `std` itself
/// allocated before this allocator was installed, and were the balance
/// unsigned that free would underflow it and the next allocation would panic
/// on the overflowing add — a panic inside the global allocator, which the
/// runtime can only answer by aborting the process.
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
/// Highest [`LIVE_BYTES`] since the last [`reset_peak`].
static PEAK_BYTES: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static EXCLUDED: Cell<bool> = const { Cell::new(false) };
}

#[doc(hidden)]
#[must_use]
#[derive(Debug)]
pub struct ThreadExclusion {
    previous: bool,
}

impl Drop for ThreadExclusion {
    fn drop(&mut self) {
        let previous = self.previous;
        let _ = EXCLUDED.try_with(|excluded| excluded.set(previous));
    }
}

#[doc(hidden)]
pub fn exclude_current_thread() -> ThreadExclusion {
    let previous = EXCLUDED.with(|excluded| excluded.replace(true));
    ThreadExclusion { previous }
}

fn excluded() -> bool {
    EXCLUDED.try_with(Cell::get).unwrap_or(false)
}

/// Live heap bytes this process currently holds, and zero while the balance
/// stands below where it started.
#[must_use]
pub fn live_bytes() -> usize {
    above_zero(LIVE_BYTES.load(Ordering::Relaxed))
}

/// Highest live heap since the last [`reset_peak`].
#[must_use]
pub fn peak_bytes() -> usize {
    above_zero(PEAK_BYTES.load(Ordering::Relaxed))
}

/// A balance as a count of bytes: what it holds, or nothing once it has fallen
/// past where the counter started.
fn above_zero(balance: isize) -> usize {
    balance.max(0).unsigned_abs()
}

/// Drop the recorded peak to what is live now.
pub fn reset_peak() {
    PEAK_BYTES.store(LIVE_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Bytes still live after `build` returns, and the peak it passed through.
///
/// The value stays alive across the measurement and is handed back, so a
/// caller measures what a subsystem *holds*, not what it touched and freed.
pub fn measure<T, Build: FnOnce() -> T>(build: Build) -> (T, Heap) {
    let before = live_bytes();
    reset_peak();
    let built = build();
    let heap = Heap {
        held: live_bytes().saturating_sub(before),
        peak: peak_bytes().saturating_sub(before),
    };
    (built, heap)
}

/// What one measured construction cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Heap {
    /// Bytes still held when the construction returned.
    pub held: usize,
    /// Highest live heap reached while it ran.
    pub peak: usize,
}

impl Heap {
    /// Render as whole kilobytes, for a one-line-per-subsystem report.
    #[must_use]
    pub fn kib(&self) -> (usize, usize) {
        (self.held / 1024, self.peak / 1024)
    }
}

/// The system allocator, counting live and peak bytes.
pub struct Counting;

// SAFETY: every method forwards to `System` with the layout it was given and
// returns exactly what `System` returned; the counters only observe.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            charge(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            charge(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        release(layout.size());
        // SAFETY: forwarded verbatim to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded verbatim to the system allocator.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if let Some(grown) = new_size.checked_sub(layout.size()) {
                charge(grown);
            } else {
                release(layout.size() - new_size);
            }
        }
        moved
    }
}

/// Add `bytes` to the live total and raise the peak when that is a new high.
fn charge(bytes: usize) {
    if excluded() {
        return;
    }
    let bytes = bytes as isize;
    let live = LIVE_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .wrapping_add(bytes);
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

/// Take `bytes` off the live total.
fn release(bytes: usize) {
    if excluded() {
        return;
    }
    LIVE_BYTES.fetch_sub(bytes as isize, Ordering::Relaxed);
}
