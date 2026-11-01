#![cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]

use std::{cell::Cell, fmt, hint::black_box};

use kithara_test_utils::{
    flight,
    memory::{self, Counting},
};
use tracing_subscriber::layer::SubscriberExt;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

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
        let (_, folded) = memory::measure(|| fire(&calls, "left"));
        let (_, unique) = memory::measure(|| fire(&calls, "right"));
        let (control, allocated) = memory::measure(|| black_box(String::with_capacity(515)));
        assert!(control.capacity() >= 515);
        assert!(allocated.held >= 515 && allocated.peak >= 515);
        drop(control);
        assert_eq!(calls.get(), 3);
        assert_eq!(folded.held, 0);
        assert_eq!(folded.peak, 0);
        assert!(unique.held > 0 && unique.peak >= unique.held);
        let tail = flight::tail();
        assert_eq!(tail.len(), 2);
        assert!(tail[0].ends_with("(x2)"));
        assert!(!tail[1].ends_with("(x2)"));
        assert!(tail[0].contains("kithara_flight_allocation_control"));
        assert!(tail[1].contains("kithara_flight_allocation_control"));
    });
}
