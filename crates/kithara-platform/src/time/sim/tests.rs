use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    thread,
    time::Instant as RealInstant,
};

use super::{Duration, Instant, advance, now_nanos, reset, sched};

/// Serialize cases that share the process-global `SIM_NANOS` and `SCHED`.
/// nextest gives each test its own process, but a thread-based runner would race
/// without this. Poison-tolerant: a panicking case must surface its own real
/// assertion, not a `PoisonError` cascade across every later case.
static GUARD: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    GUARD.lock().unwrap_or_else(PoisonError::into_inner)
}

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Generous real-time bound: every engine test below must return in ~0 real
/// time because nothing blocks on the wall clock. A bug that stops the engine
/// advancing would hang; this only exists so a hung join surfaces as a slow
/// test rather than masking a bug — it is never reached on the correct path.
fn assert_fast(start: RealInstant) {
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "engine wait consumed real wall-clock time (likely a stalled advance)"
    );
}

/// Spawn one parked waiter per duration, establishing the quiescence baseline
/// the way real participating roots do: a coordinator participant is held on the
/// main thread so `active` never reaches zero — and the clock therefore never
/// advances — until EVERY worker has registered and parked. Dropping the
/// coordinator is then the single `active -> 0` edge, so the advance sees the
/// full multiset of deadlines at once (mirrors the locked register-once contract
/// rather than racing per-thread register/park windows). Joins all workers.
fn run_parked_waiters(secs: &[u64]) {
    let coordinator = sched::register_participant();
    let handles: Vec<_> = secs
        .iter()
        .copied()
        .map(|s| {
            thread::spawn(move || {
                let _p = sched::register_participant();
                sched::park_for(Duration::from_secs(s));
            })
        })
        .collect();

    while sched::timed_count() != secs.len() {
        thread::yield_now();
    }
    drop(coordinator);

    for h in handles {
        h.join().expect("waiter thread panicked");
    }
}

#[test]
fn now_advances_only_on_advance() {
    let _g = guard();
    reset();
    let t0 = Instant::now();
    assert_eq!(Instant::now(), t0, "clock frozen between advances");
    advance(Duration::from_millis(10));
    let t1 = Instant::now();
    assert_eq!(t1.duration_since(t0), Duration::from_millis(10));
    assert!(t1 > t0);
}

#[test]
fn elapsed_tracks_virtual_clock() {
    let _g = guard();
    reset();
    let start = Instant::now();
    assert_eq!(start.elapsed(), Duration::ZERO);
    advance(Duration::from_secs(5));
    assert_eq!(start.elapsed(), Duration::from_secs(5));
}

#[test]
fn arithmetic_saturates_and_orders() {
    let _g = guard();
    reset();
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let earlier = now - Duration::from_secs(1);
    assert!(earlier < now && now < later);
    assert_eq!(later - now, Duration::from_secs(1));
    assert_eq!(later.duration_since(now), Duration::from_secs(1));
    assert_eq!(now.saturating_duration_since(later), Duration::ZERO);
}

#[test]
fn base_keeps_backward_offset_positive() {
    let _g = guard();
    reset();
    let now = Instant::now();
    let earlier = now - Duration::from_secs(3600);
    assert_eq!(now.duration_since(earlier), Duration::from_secs(3600));
}

#[test]
fn quiescence_advances_to_max_deadline_fast() {
    let _g = guard();
    reset();
    let base = now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters(&[10, 3]);

    assert_fast(real_start);
    assert_eq!(
        now_nanos(),
        base + 10 * NANOS_PER_SEC,
        "clock lands on the max registered deadline"
    );
    assert_eq!(
        sched::advance_log(),
        vec![base + 3 * NANOS_PER_SEC, base + 10 * NANOS_PER_SEC],
        "advance jumps to the min deadline first, then the next, regardless of start order"
    );
}

#[test]
fn equal_deadlines_wake_in_one_step() {
    let _g = guard();
    reset();
    let base = now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters(&[5, 5, 8]);

    assert_fast(real_start);
    assert_eq!(
        sched::advance_log(),
        vec![base + 5 * NANOS_PER_SEC, base + 8 * NANOS_PER_SEC],
        "three waiters, two advance steps: the two equal deadlines wake in one batch"
    );
    assert_eq!(now_nanos(), base + 8 * NANOS_PER_SEC);
}

#[test]
fn sequence_is_deterministic_across_runs() {
    let _g = guard();
    let run = || {
        reset();
        let base = now_nanos();
        run_parked_waiters(&[7, 2, 11, 4]);
        (base, sched::advance_log())
    };

    let real_start = RealInstant::now();
    let (base_a, log_a) = run();
    let (base_b, log_b) = run();
    assert_fast(real_start);

    assert_eq!(
        base_a, base_b,
        "reset restores the same timeline origin each run"
    );
    assert_eq!(log_a, log_b, "advance sequence is identical run-to-run");
    assert_eq!(
        log_a,
        vec![
            base_a + 2 * NANOS_PER_SEC,
            base_a + 4 * NANOS_PER_SEC,
            base_a + 7 * NANOS_PER_SEC,
            base_a + 11 * NANOS_PER_SEC,
        ],
        "distinct deadlines advance in sorted order"
    );
}

#[test]
fn participant_drop_without_park_advances_waiter() {
    let _g = guard();
    reset();
    let base = now_nanos();
    let real_start = RealInstant::now();

    // Main holds a participant first, so the waiter's own park sees `active != 0`
    // and does NOT self-advance: the only `active -> 0` edge is main's drop below.
    let main_p = sched::register_participant();

    let waiter = thread::spawn(move || {
        let _p = sched::register_participant();
        sched::park_for(Duration::from_secs(4));
    });

    // Spin until the waiter has registered AND parked: timed waiter present and
    // `active` back down to just main's participant.
    while sched::timed_count() != 1 || sched::active_count() != 1 {
        thread::yield_now();
    }

    // Dropping main's (non-parking) participant is the transition that brings
    // `active` to zero and fires the advance — proving the Participant::drop path.
    drop(main_p);

    waiter.join().expect("waiter thread panicked");

    assert_fast(real_start);
    assert_eq!(
        now_nanos(),
        base + 4 * NANOS_PER_SEC,
        "drop-driven advance woke the waiter"
    );
    assert_eq!(sched::advance_log(), vec![base + 4 * NANOS_PER_SEC]);
}
