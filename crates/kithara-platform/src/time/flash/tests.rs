use std::{
    future::Future,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
    thread,
    time::Instant as RealInstant,
};

use super::{Duration, Instant, advance, now_nanos, participate, reset, sched};
use crate::sync::Notify;

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

/// Run `body` as a quiescence participant the way the production spawn bracket
/// does: `reset_credit()` at entry (a pooled OS thread must not inherit a stale
/// credit) and `on_participant_exit()` at exit (a thread that woke to `Running`
/// and exits drops its `active` slot). The harness uses this everywhere a real
/// spawn would, since `std::thread::spawn` in tests has no platform bracket.
fn bracketed<F: FnOnce()>(body: F) {
    sched::reset_credit();
    // The harness threads stand in for dedicated `spawn_named` pacers, so they must
    // be counted in `active` exactly as production worker threads are.
    sched::mark_dedicated();
    body();
    sched::on_participant_exit();
}

/// Spawn one parked waiter per duration. Under lazy-credit a thread is invisible
/// until its first wrapped wait, so to batch the multiset deterministically a
/// test-only coordinator holds one RUNNING slot until EVERY worker has parked;
/// dropping it is the single `active -> 0` edge and the advance sees all the
/// deadlines at once. Each worker is `bracketed` so its wake-to-`Running`-then-
/// exit drops its slot (no `active` leak). Joins all workers.
fn run_parked_waiters(secs: &[u64]) {
    let coordinator = sched::test_hold();
    let handles: Vec<_> = secs
        .iter()
        .copied()
        .map(|s| thread::spawn(move || bracketed(|| sched::park_for(Duration::from_secs(s)))))
        .collect();

    while sched::timed_count() != secs.len() {
        thread::yield_now();
    }
    drop(coordinator);

    for h in handles {
        h.join().expect("waiter thread panicked");
    }
}

/// No-op waker for manually polling a future without a runtime.
struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

/// A parked async task that gets signalled must stay counted as non-quiescent
/// until it is actually RE-POLLED — not merely until its current poll returns.
/// Between `waker.wake()` (task queued/runnable) and the next `poll`, the engine
/// must NOT advance the virtual clock, or it starves the runnable task: the HLS
/// seek-test `DecodeError::Interrupted` root cause (a parked sync reader's budget
/// burns while a runnable download task never runs in virtual time). Drives the
/// production `participate` poll-wrapper directly — no runtime.
#[test]
fn woken_async_task_stays_counted_until_repolled() {
    let _g = guard();
    reset();

    let notify = Notify::new();
    let mut task = Box::pin(participate(async {
        notify.notified().await;
    }));
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);

    // First poll: the task registers on the notify and parks. A genuinely parked
    // task (waker not yet fired) is correctly uncounted.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert_eq!(sched::async_active_count(), 0, "parked task is uncounted");

    // Signal it. The task is now RUNNABLE (its waker fired, it is queued) but has
    // NOT been re-polled. It MUST be counted so a concurrent `try_advance` cannot
    // jump the clock past it.
    notify.notify_one();
    assert_eq!(
        sched::async_active_count(),
        1,
        "a woken-but-not-yet-repolled task must keep the engine non-quiescent"
    );

    // Re-poll resolves it; the slot is released exactly once.
    assert!(task.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        sched::async_active_count(),
        0,
        "a completed task releases its slot exactly once"
    );
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
fn first_park_bootstraps_and_self_advances() {
    let _g = guard();
    reset();
    let base = now_nanos();
    let real_start = RealInstant::now();

    // No coordinator: a lone thread is uncounted (Credit::None) until its first
    // wrapped wait. That first park bootstraps to `Parked` WITHOUT touching
    // `active`, so `active` is already 0 — the engine advances immediately to the
    // waiter's deadline. This is the lazy-credit bootstrap edge, the replacement
    // for the old explicit-register/drop transition.
    let waiter = thread::spawn(move || bracketed(|| sched::park_for(Duration::from_secs(4))));

    waiter.join().expect("waiter thread panicked");

    assert_fast(real_start);
    assert_eq!(
        now_nanos(),
        base + 4 * NANOS_PER_SEC,
        "first-park bootstrap self-advanced the lone waiter"
    );
    assert_eq!(sched::advance_log(), vec![base + 4 * NANOS_PER_SEC]);
    assert_eq!(sched::active_count(), 0, "bootstrap balanced on exit");
}

/// Deterministic xorshift so the stress mix is reproducible run-to-run without
/// pulling a dependency into the platform crate.
fn xs(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Adversarial multi-threaded stress over EVERY engine wait path with racing
/// wakers AND the lazy-credit edge cases, run for many rounds. Proves the
/// all-under-`SCHED` protocol never underflows `active` (a `debug_assert` would
/// panic) and never loses a wakeup (a lost wake hangs → the join never completes
/// → the generous `assert_fast` bound surfaces it as a stalled test rather than
/// a silent pass), and that lazy credit always nets to zero.
///
/// Each round, alongside `K` mixed-kind waiters, three lazy-credit-specific
/// workers run concurrently:
///   - **busy-spin**: never enters a wrapped wait (just yields a bounded count
///     then exits). It must stay `Credit::None` → uncounted → invisible to the
///     engine; it must NOT stall the clock and must NOT touch `active`.
///   - **decoder-build-like**: does N condvar waits with racing signals, then
///     EXITS while last-woken (`Running`), wrapped by the spawn-bracket
///     (`bracketed`) so the exit drops its `active` slot — proving no leak from
///     the bootstrap-then-run-then-exit path the production `spawn_blocking`
///     bracket covers.
///   - **scheduler-like**: a park/wake loop (`park_timed_unparkable` raced by
///     `unpark`) then exits — mirrors the audio worker.
///
/// The `K` mixed waiters cover:
///   - Thread park raced by a sibling `unpark` (either ordering) — self-releases
///     via its deadline even when the racing unpark is lost, so it never hangs.
///   - Timed condvar raced by `signal_condvar` — self-releases via its deadline.
///   - Untimed condvar (no deadline) — released by the main thread re-signalling
///     each registered cvid, the register-then-signal ordering storage relies on.
///
/// A test-only coordinator (`test_hold`) holds one RUNNING slot until the spin
/// settles, so a round exercises a realistic mix of running and parked threads.
/// Every spawned body is `bracketed` (reset credit on entry, drop on exit) the
/// way the production spawn bracket does.
#[test]
fn stress_mixed_waits_no_underflow_no_lost_wakeup() {
    const ROUNDS: u64 = 16;
    const K: u64 = 5;
    const SPIN_WAITS: u64 = 3;

    let _g = guard();
    let real_start = RealInstant::now();

    for round in 0..ROUNDS {
        reset();
        let coordinator = sched::test_hold();

        // Untimed waiters are released by the main thread after they register.
        // Collect their cvids to re-signal, and track how many have not yet woken
        // so the main thread keeps signalling until EVERY untimed worker has been
        // released. Polling `indef_count() == 0` alone is racy: a slow untimed
        // worker may not have called `register_condvar_untimed` when the loop
        // first observes an empty map, so it would exit before that worker parks
        // and then the worker would wait forever. The counter closes that window —
        // it starts non-zero and only the worker itself decrements it post-wake.
        let mut untimed_cvids: Vec<u64> = Vec::new();
        let untimed_left = Arc::new(AtomicUsize::new(0));

        // Busy-spin worker: NEVER waits. Must stay uncounted and never stall the
        // engine (no deadline, not in `active`). `bracketed` confirms exit is a
        // no-op when credit stayed `None`.
        let spin = thread::spawn(|| {
            bracketed(|| {
                for _ in 0..64 {
                    thread::yield_now();
                }
            });
        });

        // Decoder-build-like worker: N condvar waits with racing signals, exits
        // while `Running`. The bracket's `on_participant_exit` drops the slot.
        let builder = {
            let mut seed = round.wrapping_mul(2_654_435_761).wrapping_add(7);
            thread::spawn(move || {
                bracketed(|| {
                    for _ in 0..SPIN_WAITS {
                        let cvid = sched::next_condvar_id();
                        let deadline = now_nanos() + NANOS_PER_SEC;
                        let (token, adv) = sched::register_condvar_timed(deadline, cvid);
                        let racer = thread::spawn(move || {
                            if xs(&mut seed) & 1 == 0 {
                                thread::yield_now();
                            }
                            sched::signal_condvar(cvid, true);
                        });
                        sched::fire_advance(adv);
                        token.wait();
                        sched::mark_running_after_condvar();
                        racer.join().expect("builder signal racer panicked");
                    }
                    // Exit here while `Running`: the bracket must decrement.
                });
            })
        };

        // Scheduler-like worker: park/wake loop raced by `unpark`, then exits.
        let scheduler = {
            let mut seed = round.wrapping_mul(40_503).wrapping_add(13);
            thread::spawn(move || {
                bracketed(|| {
                    let me = crate::thread::current_thread_id();
                    for _ in 0..SPIN_WAITS {
                        let racer = thread::spawn(move || {
                            if xs(&mut seed) & 1 == 0 {
                                thread::yield_now();
                            }
                            sched::unpark(me);
                        });
                        sched::park_timed_unparkable(Duration::from_secs(2), me);
                        racer.join().expect("scheduler unpark racer panicked");
                    }
                });
            })
        };

        let handles: Vec<_> = (0..K)
            .map(|idx| {
                let kind = (round.wrapping_mul(31).wrapping_add(idx)) % 3;
                let cvid = sched::next_condvar_id();
                if kind == 2 {
                    untimed_cvids.push(cvid);
                    untimed_left.fetch_add(1, Ordering::Relaxed);
                }
                let mut seed = round.wrapping_mul(1_000_003).wrapping_add(idx + 1);
                let left = Arc::clone(&untimed_left);
                thread::spawn(move || {
                    bracketed(|| match kind {
                        0 => {
                            // Thread park; a sibling racing unpark targets the
                            // SAME engine thread key. Either ordering is safe;
                            // the deadline guarantees return regardless.
                            let me = crate::thread::current_thread_id();
                            let racer = thread::spawn(move || {
                                if xs(&mut seed) & 1 == 0 {
                                    thread::yield_now();
                                }
                                sched::unpark(me);
                            });
                            sched::park_timed_unparkable(Duration::from_secs(1 + (idx % 4)), me);
                            racer.join().expect("unpark racer panicked");
                        }
                        1 => {
                            // Timed condvar raced by a sibling signal.
                            let deadline = now_nanos() + (1 + (idx % 4)) * NANOS_PER_SEC;
                            let (token, adv) = sched::register_condvar_timed(deadline, cvid);
                            sched::fire_advance(adv);
                            let racer = thread::spawn(move || {
                                if xs(&mut seed) & 1 == 0 {
                                    thread::yield_now();
                                }
                                sched::signal_condvar(cvid, true);
                            });
                            token.wait();
                            sched::mark_running_after_condvar();
                            racer.join().expect("signal racer panicked");
                        }
                        _ => {
                            // Untimed condvar: released by the main thread only.
                            let (token, adv) = sched::register_condvar_untimed(cvid);
                            sched::fire_advance(adv);
                            token.wait();
                            sched::mark_running_after_condvar();
                            left.fetch_sub(1, Ordering::Relaxed);
                        }
                    });
                })
            })
            .collect();

        // Drop the coordinator. Timed/thread waiters self-release via their
        // deadlines once the engine reaches quiescence. Untimed waiters have NO
        // deadline, so keep re-signalling each registered cvid until every untimed
        // worker has woken (`untimed_left == 0`). Signalling an unregistered or
        // already-woken cvid is a harmless no-op, never a lost wakeup for an
        // already-parked waiter; the counter (not `indef_count`) is the loop
        // condition so a worker still en route to its `register` cannot make the
        // loop exit early.
        drop(coordinator);
        while untimed_left.load(Ordering::Relaxed) != 0 {
            for cvid in &untimed_cvids {
                sched::signal_condvar(*cvid, true);
            }
            thread::yield_now();
        }

        spin.join().expect("busy-spin worker panicked");
        builder.join().expect("decoder-build-like worker panicked");
        scheduler.join().expect("scheduler-like worker panicked");
        for h in handles {
            h.join().expect("stress worker panicked");
        }

        // Engine fully drained at the end of every round: no leaked waiters, no
        // residual running participant.
        assert_eq!(sched::active_count(), 0, "round {round}: active leaked");
        assert_eq!(
            sched::timed_count(),
            0,
            "round {round}: timed waiter leaked"
        );
        assert_eq!(
            sched::indef_count(),
            0,
            "round {round}: indef waiter leaked"
        );
    }

    // This test spawns hundreds of OS threads (per round: the mixed waiters, the
    // three lazy-credit workers, and one racer each), so its wall-time is
    // dominated by real thread spawn/join, NOT engine waits — every virtual wait
    // still collapses to zero. The bound is a hang ceiling: a genuine lost wakeup
    // stalls a join indefinitely (60s+ before this fires), far above the
    // thread-churn cost. It is deliberately looser than `assert_fast`'s 2s, which
    // guards the single-purpose engine tests that spawn only a handful of threads.
    assert!(
        real_start.elapsed() < Duration::from_secs(30),
        "stress mix stalled (a lost wakeup hangs a join far beyond thread-churn cost)"
    );
}

/// The advance SEQUENCE is a pure function of the deadline multiset, independent
/// of thread scheduling — so a fixed scenario produces a byte-identical
/// `advance_log` and final clock across repeated `reset()`+run. Uses only timed
/// condvar waiters (deterministic deadlines, no racing wake) so the only source
/// of clock motion is the engine's min-jump rule.
#[test]
fn stress_advance_log_is_deterministic_across_runs() {
    let _g = guard();
    let secs = [6_u64, 2, 9, 2, 4, 9, 1];
    let real_start = RealInstant::now();

    let run = || {
        reset();
        let base = now_nanos();
        // The coordinator holds the clock at `base` until every worker has parked,
        // so each worker reads the same `base` for its absolute deadline (the
        // multiset is `base + s` for each `s`). Workers are `bracketed` so wake-to-
        // `Running`-then-exit balances `active` and the engine drains to zero.
        let coordinator = sched::test_hold();
        let handles: Vec<_> = secs
            .iter()
            .copied()
            .map(|s| {
                thread::spawn(move || {
                    bracketed(|| {
                        let cvid = sched::next_condvar_id();
                        let deadline = now_nanos() + s * NANOS_PER_SEC;
                        let (token, adv) = sched::register_condvar_timed(deadline, cvid);
                        sched::fire_advance(adv);
                        token.wait();
                        sched::mark_running_after_condvar();
                    });
                })
            })
            .collect();
        while sched::timed_count() != secs.len() {
            thread::yield_now();
        }
        drop(coordinator);
        for h in handles {
            h.join().expect("waiter panicked");
        }
        (base, now_nanos(), sched::advance_log())
    };

    let (base_a, end_a, log_a) = run();
    let (base_b, end_b, log_b) = run();
    assert_fast(real_start);

    assert_eq!(base_a, base_b, "reset restores the same origin");
    assert_eq!(log_a, log_b, "advance sequence identical run-to-run");
    // Distinct deadlines (1,2,4,6,9) jumped to in sorted order; the two 2s and
    // two 9s batch into a single step each, so five distinct steps.
    assert_eq!(
        log_a,
        vec![
            base_a + NANOS_PER_SEC,
            base_a + 2 * NANOS_PER_SEC,
            base_a + 4 * NANOS_PER_SEC,
            base_a + 6 * NANOS_PER_SEC,
            base_a + 9 * NANOS_PER_SEC,
        ],
        "min-jump over the deadline multiset",
    );
    assert_eq!(
        end_a,
        base_a + 9 * NANOS_PER_SEC,
        "clock lands on max deadline"
    );
    assert_eq!(end_b, base_b + 9 * NANOS_PER_SEC);
}
