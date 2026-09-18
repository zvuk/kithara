//! Memory contract of the tracing USDT backend: probes fired without pause
//! keep the heap bounded whether nothing observes them or a scope records
//! them far past its history cap.

use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Barrier,
    thread,
};

use kithara_test_utils::{
    memory::{self, Counting},
    test::{
        setup_tracing,
        usdt::{MAX_EVENTS, ProbeEvent, scope},
    },
    tracing::{Level, event},
};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const THREADS: usize = 4;
/// Firings per thread before measuring: fills the flight recorder's rings and
/// every per-thread tracing buffer, which are bounded but not preallocated.
const WARMUP: u64 = 20_000;
/// Firings per thread measured: far past every bound the backend keeps.
const FIRINGS: u64 = 1_000_000;
/// Growth allowed once warm while nothing keeps history: allocator and
/// ring-entry jitter, never a per-firing cost.
const STEADY_BUDGET: usize = 256 * 1024;

fn fire(probe: &'static str, value: u64) {
    event!(
        target: "kithara_test_probe",
        Level::TRACE,
        probe = probe,
        value = value,
    );
}

/// Warms up on `warm_probe` then fires `probe` from [`THREADS`] threads,
/// returning the peak heap growth over the warm baseline and the growth left
/// after the threads finish.
///
/// `warm_probe` is `None` once the backend is already warm from an earlier
/// call. It has to be: a warm-up fires real probes, and a scope standing at
/// the time records them into the history of the probe it warms on, so
/// warming again would grow that history before the baseline is taken and
/// leave the measurement reading what was left to grow rather than what the
/// history costs.
fn hammer(warm_probe: Option<&'static str>, probe: &'static str) -> (usize, usize) {
    let warm = Barrier::new(THREADS + 1);
    let go = Barrier::new(THREADS + 1);
    let mut baseline = 0;
    thread::scope(|threads| {
        for _ in 0..THREADS {
            threads.spawn(|| {
                if let Some(warm_probe) = warm_probe {
                    for value in 0..WARMUP {
                        fire(warm_probe, value);
                    }
                }
                warm.wait();
                go.wait();
                for value in 0..FIRINGS {
                    fire(probe, value);
                }
            });
        }
        warm.wait();
        baseline = memory::live_bytes();
        memory::reset_peak();
        go.wait();
    });
    let peak = memory::peak_bytes().saturating_sub(baseline);
    let left = memory::live_bytes().saturating_sub(baseline);
    (peak, left)
}

/// Loads the symbol cache std keeps for the rest of the process the first
/// time a panic prints a backtrace, so the overflow panic below cannot read
/// as heap a dropped scope left behind.
fn prime_panic_backtrace() {
    let primed = catch_unwind(|| panic!("priming the panic backtrace cache"));
    assert!(primed.is_err());
}

#[test]
fn continuous_probes_keep_the_heap_bounded() {
    setup_tracing();
    prime_panic_backtrace();

    let (peak, left) = hammer(Some("unobserved"), "unobserved");
    eprintln!("unobserved: peak +{peak} B, left +{left} B");
    assert!(
        peak <= STEADY_BUDGET,
        "unobserved probes grew the heap by {peak} B"
    );

    let before = memory::live_bytes();
    let history = scope();
    let (peak, _) = hammer(None, "history");
    let history_bytes = MAX_EVENTS * size_of::<ProbeEvent>();
    assert!(
        peak >= history_bytes,
        "a scope past MAX_EVENTS must have held the full history, peak {peak} B"
    );
    let history_budget = history_bytes + history_bytes / 2 + STEADY_BUDGET;
    eprintln!(
        "history: peak +{peak} B for {MAX_EVENTS} events of {} B (budget {history_budget} B)",
        size_of::<ProbeEvent>()
    );
    assert!(
        peak <= history_budget,
        "a scope grew the heap by {peak} B, over {history_budget} B"
    );
    let overflow = catch_unwind(AssertUnwindSafe(|| history.events().len()));
    assert!(
        overflow.is_err(),
        "a history past MAX_EVENTS must fail its reader"
    );
    assert!(
        history.last("history").is_some(),
        "an overflowed history must keep the latest firing"
    );
    drop(history);

    let left = memory::live_bytes().saturating_sub(before);
    eprintln!("after the scopes: left +{left} B");
    assert!(
        left <= STEADY_BUDGET,
        "a dropped scope left {left} B on the heap"
    );
}
