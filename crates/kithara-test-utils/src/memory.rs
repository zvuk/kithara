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
    sync::atomic::{AtomicUsize, Ordering},
};

/// Live heap bytes, maintained by [`Counting`].
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
/// Highest [`LIVE_BYTES`] since the last [`reset_peak`].
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Live heap bytes this process currently holds.
#[must_use]
pub fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Highest live heap since the last [`reset_peak`].
#[must_use]
pub fn peak_bytes() -> usize {
    PEAK_BYTES.load(Ordering::Relaxed)
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
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
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
                LIVE_BYTES.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        moved
    }
}

/// Add `bytes` to the live total and raise the peak when that is a new high.
fn charge(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}
