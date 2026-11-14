#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]

use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    fmt,
    hint::black_box,
    sync::Barrier,
    thread,
};

use kithara_test_utils::{
    flight,
    memory::{self, Counting},
};
use tracing_subscriber::layer::SubscriberExt;

#[global_allocator]
static ALLOCATOR: ObservedCounting = ObservedCounting;

thread_local! {
    static EVENT_ALLOCATION: Cell<Option<bool>> = const { Cell::new(None) };
}

struct ObservedCounting;

fn allocation_call() {
    let _ = EVENT_ALLOCATION.try_with(|seen| {
        if seen.get().is_some() {
            seen.set(Some(true));
        }
    });
}

// SAFETY: every allocation operation forwards its original arguments to Counting.
unsafe impl GlobalAlloc for ObservedCounting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocation_call();
        // SAFETY: GlobalAlloc supplied the layout passed to Counting unchanged.
        unsafe { Counting.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocation_call();
        // SAFETY: GlobalAlloc supplied the layout passed to Counting unchanged.
        unsafe { Counting.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the pointer and its original layout are forwarded to Counting.
        unsafe { Counting.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocation_call();
        // SAFETY: the pointer, layout and new size are forwarded to Counting.
        unsafe { Counting.realloc(ptr, layout, size) }
    }
}

fn observe_event(emit: impl FnOnce()) -> (memory::Heap, bool) {
    EVENT_ALLOCATION.with(|seen| assert!(seen.replace(Some(false)).is_none()));
    let (_, heap) = memory::measure(emit);
    let allocated = EVENT_ALLOCATION
        .with(|seen| seen.replace(None))
        .expect("the event allocation observation is active");
    (heap, allocated)
}

struct Payload<'a> {
    calls: &'a Cell<usize>,
    ending: &'a str,
}

impl fmt::Debug for Payload<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.calls.set(self.calls.get() + 1);
        for _ in 0..512 {
            f.write_str("\u{1f980}")?;
        }
        f.write_str(self.ending)
    }
}

fn fire(calls: &Cell<usize>, ending: &str) {
    tracing::debug!(
        target: "kithara_flight_allocation_control",
        payload = ?Payload { calls, ending },
    );
}

#[test]
fn folded_flight_events_do_not_allocate_and_unique_events_still_do() {
    let subscriber = tracing_subscriber::registry().with(flight::layer());
    tracing::subscriber::with_default(subscriber, || {
        let calls = Cell::new(0);
        fire(&calls, "left");
        let gate = Barrier::new(2);
        let (folded, folded_thread_allocated) = thread::scope(|scope| {
            let gate = &gate;
            scope.spawn(move || {
                gate.wait();
                gate.wait();
                let held = black_box(String::with_capacity(144));
                gate.wait();
                gate.wait();
                drop(held);
            });
            gate.wait();
            let measured = observe_event(|| {
                gate.wait();
                gate.wait();
                fire(&calls, "left");
            });
            gate.wait();
            measured
        });
        let (unique, unique_thread_allocated) = observe_event(|| fire(&calls, "right"));
        let (control, allocated) = memory::measure(|| black_box(String::with_capacity(515)));
        assert!(control.capacity() >= 515);
        assert!(allocated.held >= 515 && allocated.peak >= 515);
        drop(control);
        assert_eq!(calls.get(), 3);
        assert!(
            !folded_thread_allocated,
            "the folded emitter allocated; process-wide heap: {folded:?}"
        );
        assert!(
            folded.held >= 144 && folded.peak >= folded.held,
            "the foreign-thread allocation control was observed: {folded:?}"
        );
        assert!(unique.held > 0 && unique.peak >= unique.held);
        assert!(
            unique_thread_allocated,
            "the unique-event control allocates"
        );
        let tail = flight::tail();
        assert_eq!(tail.len(), 2);
        assert!(tail[0].ends_with("(x2)"));
        assert!(!tail[1].ends_with("(x2)"));
        assert!(tail[0].contains("kithara_flight_allocation_control"));
        assert!(tail[1].contains("kithara_flight_allocation_control"));
    });
}
