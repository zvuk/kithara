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
//! and then reads [`live_bytes`] around the construction it is measuring on
//! that thread. [`measure`] wraps that pattern: it returns what the value it
//! built holds, so a suite can name one line per subsystem instead of one total.
//! Tests that deliberately measure work across owned worker threads use
//! [`process_live_bytes`] instead.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicIsize, Ordering},
};

static PROCESS_LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static PROCESS_PEAK_BYTES: AtomicIsize = AtomicIsize::new(0);

struct Accounting {
    /// Signed because a target that links `std` dynamically can free buffers
    /// allocated before this allocator was installed; an unsigned balance would
    /// underflow and panic inside the global allocator.
    live: Cell<isize>,
    peak: Cell<isize>,
}

thread_local! {
    static ACCOUNTING: Accounting = const {
        Accounting {
            live: Cell::new(0),
            peak: Cell::new(0),
        }
    };
}

impl Accounting {
    fn live(&self) -> usize {
        above_zero(self.live.get())
    }

    fn peak(&self) -> usize {
        above_zero(self.peak.get())
    }

    fn reset_peak(&self) {
        self.peak.set(self.live.get());
    }

    fn charge(&self, bytes: usize) {
        let live = self.live.get().wrapping_add(bytes as isize);
        self.live.set(live);
        self.peak.set(self.peak.get().max(live));
    }

    fn release(&self, bytes: usize) {
        self.live.set(self.live.get().wrapping_sub(bytes as isize));
    }
}

/// Live heap bytes this thread currently holds, and zero while the balance
/// stands below where it started.
#[must_use]
pub fn live_bytes() -> usize {
    ACCOUNTING.with(Accounting::live)
}

/// Highest live heap on this thread since the last [`reset_peak`].
#[must_use]
pub fn peak_bytes() -> usize {
    ACCOUNTING.with(Accounting::peak)
}

/// A balance as a count of bytes: what it holds, or nothing once it has fallen
/// past where the counter started.
fn above_zero(balance: isize) -> usize {
    balance.max(0).unsigned_abs()
}

/// Drop the calling thread's recorded peak to what is live now.
pub fn reset_peak() {
    ACCOUNTING.with(Accounting::reset_peak);
}

/// Live heap bytes across every thread in this process.
#[must_use]
pub fn process_live_bytes() -> usize {
    above_zero(PROCESS_LIVE_BYTES.load(Ordering::Relaxed))
}

/// Highest process-wide live heap since the last [`reset_process_peak`].
#[must_use]
pub fn process_peak_bytes() -> usize {
    above_zero(PROCESS_PEAK_BYTES.load(Ordering::Relaxed))
}

/// Drop the process-wide recorded peak to what is live now.
pub fn reset_process_peak() {
    PROCESS_PEAK_BYTES.store(
        PROCESS_LIVE_BYTES.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
}

/// Bytes still live after `build` returns on the calling thread, and the peak
/// it passed through. Allocations on other threads are not included.
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

/// The system allocator, counting per-thread and process-wide heap balances.
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

/// Add `bytes` to the calling thread and process balances.
fn charge(bytes: usize) {
    let _ = ACCOUNTING.try_with(|accounting| accounting.charge(bytes));
    let bytes = bytes as isize;
    let live = PROCESS_LIVE_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .wrapping_add(bytes);
    PROCESS_PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

/// Take `bytes` off the calling thread and process live balances.
fn release(bytes: usize) {
    let _ = ACCOUNTING.try_with(|accounting| accounting.release(bytes));
    PROCESS_LIVE_BYTES.fetch_sub(bytes as isize, Ordering::Relaxed);
}
