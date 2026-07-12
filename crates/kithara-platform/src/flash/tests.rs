use std::{
    future::Future,
    panic::Location,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
    thread,
    time::Instant as RealInstant,
};

use super::{
    Duration, Instant, advance, ambient_scope, enter_dynamic, flash_enabled, participate, reset,
    system::{FlashInner, credit, sched},
    yield_now,
};
use crate::sync::{Arc, Notify};

/// Serialize the cases that drive the process-global `FLASH` engine (the
/// primitive/TLS-path tests). The pure-scheduler tests run on LOCAL
/// [`FlashInner`] instances and do not take it. nextest gives each test its
/// own process, but a thread-based runner would race without this.
/// Poison-tolerant: a panicking case must surface its own real assertion, not
/// a `PoisonError` cascade across every later case.
static GUARD: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    GUARD.lock().unwrap_or_else(PoisonError::into_inner)
}

const NANOS_PER_SEC: u64 = 1_000_000_000;
#[cfg(feature = "no-block")]
const NO_BLOCK_BRIDGED_BUDGET_MS: u64 = 10_000;
#[cfg(feature = "no-block")]
const NO_BLOCK_ENGINE_WAIT_MS: u64 = 30;
#[cfg(feature = "no-block")]
const NO_BLOCK_TIGHT_BUDGET_MS: u64 = 1;

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

/// Run `body` as a quiescence participant of `flash` the way the production
/// spawn bracket does: `reset_credit()` at entry (a pooled OS thread must not
/// inherit a stale credit) and `on_participant_exit()` at exit (a thread that
/// woke to `Running` and exits drops its `active` slot). The harness uses this
/// everywhere a real spawn would, since `std::thread::spawn` in tests has no
/// platform bracket. The credit halves (`reset_credit`/`mark_dedicated`) are
/// thread-local — one per thread, instance-agnostic — while the `active`
/// bookkeeping lands on `flash`, the SAME instance every engine call of the
/// body must use (splitting them across two engines underflows one of them).
fn bracketed_on<F: FnOnce()>(flash: &FlashInner, body: F) {
    credit::reset_credit();
    // The harness threads stand in for dedicated `spawn_named` pacers, so they must
    // be counted in `active` exactly as production worker threads are. Production
    // splits this across the spawn: `pre_count_dedicated()` reserves the `active`
    // slot on the parent BEFORE the child runs, and `mark_dedicated()` claims it
    // `Running` on the child. The test has no spawn bracket, so it does both here.
    flash.pre_count_dedicated();
    credit::mark_dedicated();
    body();
    flash.on_participant_exit();
}

/// Spawn one parked waiter per duration on `flash`. Under lazy-credit a thread
/// is invisible until its first wrapped wait, so to batch the multiset
/// deterministically a test-only coordinator holds one RUNNING slot until EVERY
/// worker has parked; dropping it is the single `active -> 0` edge and the
/// advance sees all the deadlines at once. Each worker is `bracketed_on` so its
/// wake-to-`Running`-then-exit drops its slot (no `active` leak). Joins all
/// workers.
fn run_parked_waiters_on(flash: &Arc<FlashInner>, secs: &[u64]) {
    let coordinator = flash.test_hold();
    let handles: Vec<_> = secs
        .iter()
        .copied()
        .map(|s| {
            let flash = Arc::clone(flash);
            thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_secs(s))))
        })
        .collect();

    while flash.timed_count() != secs.len() {
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

#[cfg(feature = "no-block")]
fn poll_once_no_block<F: Future>(fut: F) -> std::task::Poll<F::Output> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);

    fut.as_mut().poll(&mut cx)
}

#[cfg(feature = "no-block")]
#[test]
fn no_block_bridged_engine_wait_panics() {
    let _g = guard();
    reset();
    crate::no_block::mode::force_mode(crate::no_block::mode::Mode::Panic);
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let task = crate::no_block::watch_budget(
        "bridged_engine_wait",
        NO_BLOCK_BRIDGED_BUDGET_MS,
        participate(
            async {
                crate::thread::park_timeout(Duration::from_millis(NO_BLOCK_ENGINE_WAIT_MS));
            },
            Location::caller(),
        ),
    );

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _ = poll_once_no_block(task);
    }))
    .expect_err("bridged engine wait must panic in no-block panic mode");
    let msg = err
        .as_ref()
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.as_ref().downcast_ref::<&'static str>().copied())
        .unwrap_or("");

    assert!(msg.contains("BRIDGED"), "panic message: {msg}");
}

#[cfg(feature = "no-block")]
#[test]
fn no_block_permit_poll_suppresses_bridged_wait_and_budget() {
    let _g = guard();
    reset();
    crate::no_block::mode::force_mode(crate::no_block::mode::Mode::Panic);
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let task = crate::no_block::watch_budget(
        "permitted_bridged_engine_wait",
        NO_BLOCK_TIGHT_BUDGET_MS,
        participate(
            crate::no_block::permit_poll(async {
                crate::thread::park_timeout(Duration::from_millis(NO_BLOCK_ENGINE_WAIT_MS));
            }),
            Location::caller(),
        ),
    );

    assert!(poll_once_no_block(task).is_ready());
}

#[test]
fn flash_active_defaults_real_and_nests() {
    let _g = guard();
    assert!(!flash_enabled(), "default must be real (`active` flag off)");
    {
        let _a = ambient_scope(true); // ambient must be on for dynamic to take
        let _g2 = enter_dynamic(true);
        assert!(
            flash_enabled(),
            "inside an ambient+dynamic scope flash is active"
        );
        {
            let _real = enter_dynamic(false);
            assert!(!flash_enabled(), "flash(false) carves real");
        }
        assert!(flash_enabled(), "restored on carve drop");
    }
    assert!(!flash_enabled(), "restored on scope drop");
}

#[test]
fn dynamic_is_noop_without_ambient() {
    let _g = guard();
    let _d = enter_dynamic(true);
    assert!(!flash_enabled(), "dynamic flash without ambient stays real");
}

/// The `restore_mode` LIFO guard must catch a non-LIFO mode-scope drop (a
/// scope restored while a later-created scope is still alive) instead of
/// silently resurrecting a stale mode. The interleave must be value-visible:
/// a same-value interleave is invisible to the value-compare assert by
/// construction, hence `true` vs `false`. Mode is pure TLS (no engine state),
/// so no GUARD; `feature = "flash"` is structural (the whole module is gated
/// in `lib.rs`), only the assert needs `debug_assertions`.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "non-LIFO mode-scope drop")]
fn non_lifo_mode_scope_drop_is_caught() {
    let outer = ambient_scope(true);
    let _inner = ambient_scope(false);
    drop(outer);
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

    let notify = Notify::default();
    let mut task = Box::pin(participate(
        async {
            notify.notified().await;
        },
        Location::caller(),
    ));
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);

    // First poll: the task registers on the notify and parks. A genuinely parked
    // task (waker not yet fired) is correctly uncounted.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert_eq!(sched::async_active_count(), 0, "parked task is uncounted");

    // Signal it. The task is now RUNNABLE (its waker fired, it is queued) but has
    // NOT been re-polled. It MUST be counted so a concurrent `try_advance` cannot
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
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
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
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let start = Instant::now();
    assert_eq!(start.elapsed(), Duration::ZERO);
    advance(Duration::from_secs(5));
    assert_eq!(start.elapsed(), Duration::from_secs(5));
}

// The two arithmetic tests below are pure `Instant` math: no ambient, so
// `Instant::now()` is the REAL arm (a lock-free monotonic read off the
// process clock anchor) and no engine state — global or local — is touched.
// Instance-agnostic, hence no GUARD, no reset, no local engine.
#[test]
fn arithmetic_saturates_and_orders() {
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
    let now = Instant::now();
    let earlier = now - Duration::from_secs(3600);
    assert_eq!(now.duration_since(earlier), Duration::from_secs(3600));
}

#[test]
fn quiescence_advances_to_max_deadline_fast() {
    let flash = FlashInner::new_arc();
    let base = flash.clock.now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters_on(&flash, &[10, 3]);

    assert_fast(real_start);
    assert_eq!(
        flash.clock.now_nanos(),
        base + 10 * NANOS_PER_SEC,
        "clock lands on the max registered deadline"
    );
    assert_eq!(
        flash.advance_log(),
        vec![base + 3 * NANOS_PER_SEC, base + 10 * NANOS_PER_SEC],
        "advance jumps to the min deadline first, then the next, regardless of start order"
    );
}

#[test]
fn equal_deadlines_wake_in_one_step() {
    let flash = FlashInner::new_arc();
    let base = flash.clock.now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters_on(&flash, &[5, 5, 8]);

    assert_fast(real_start);
    assert_eq!(
        flash.advance_log(),
        vec![base + 5 * NANOS_PER_SEC, base + 8 * NANOS_PER_SEC],
        "three waiters, two advance steps: the two equal deadlines wake in one batch"
    );
    assert_eq!(flash.clock.now_nanos(), base + 8 * NANOS_PER_SEC);
}

#[test]
fn sequence_is_deterministic_across_runs() {
    let flash = FlashInner::new_arc();
    let run = || {
        flash.reset();
        let base = flash.clock.now_nanos();
        run_parked_waiters_on(&flash, &[7, 2, 11, 4]);
        (base, flash.advance_log())
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
    let flash = FlashInner::new_arc();
    let base = flash.clock.now_nanos();
    let real_start = RealInstant::now();

    // No coordinator: a lone thread is uncounted (Credit::None) until its first
    // wrapped wait. That first park bootstraps to `Parked` WITHOUT touching
    // `active`, so `active` is already 0 — the engine advances immediately to the
    // waiter's deadline. This is the lazy-credit bootstrap edge, the replacement
    // for the old explicit-register/drop transition.
    let waiter = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_secs(4))))
    };

    waiter.join().expect("waiter thread panicked");

    assert_fast(real_start);
    assert_eq!(
        flash.clock.now_nanos(),
        base + 4 * NANOS_PER_SEC,
        "first-park bootstrap self-advanced the lone waiter"
    );
    assert_eq!(flash.advance_log(), vec![base + 4 * NANOS_PER_SEC]);
    assert_eq!(flash.active_count(), 0, "bootstrap balanced on exit");
}

#[test]
fn real_io_defers_advance_past_real_pace() {
    let flash = FlashInner::new_arc();
    let t0 = flash.clock.now_nanos();

    flash.real_io_enter();
    let waiter = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_secs(10))))
    };
    while flash.timed_count() != 1 {
        thread::yield_now();
    }

    // 50ms of REAL time passes while the op is in flight: the 10s virtual
    // deadline is far beyond the real pace, so the clock must not move.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        flash.timed_count(),
        1,
        "a virtual deadline beyond the real pace must stay parked while real I/O is in flight"
    );
    assert_eq!(
        flash.clock.now_nanos(),
        t0,
        "the clock must not advance past the real pace while real I/O is in flight"
    );

    let real_start = RealInstant::now();
    flash.real_io_exit();
    waiter.join().expect("waiter thread panicked");
    assert_fast(real_start);
    assert_eq!(
        flash.clock.now_nanos(),
        t0 + 10 * NANOS_PER_SEC,
        "full-speed collapse resumes the instant the last real I/O op completes"
    );
}

#[test]
fn real_io_paces_deadline_to_real_time_not_pin() {
    let flash = FlashInner::new_arc();
    let t0 = flash.clock.now_nanos();

    flash.real_io_enter();
    let real_start = RealInstant::now();
    let waiter = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_millis(20))))
    };

    // The deadline must fire WHILE the op is still in flight, once enough REAL
    // time has passed (pace, not pin): a virtually-delayed peer (the fixture
    // server's DelayGate) must make progress even though the client holds a
    // real-I/O scope across the whole request await — a pin would deadlock it.
    waiter.join().expect("waiter thread panicked");
    let waited = real_start.elapsed();
    flash.real_io_exit();

    assert!(
        waited >= Duration::from_millis(15),
        "deadline fired ahead of its real-time pace: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(2),
        "pace must release the deadline at ~20ms real, not pin it: {waited:?}"
    );
    assert_eq!(flash.clock.now_nanos(), t0 + 20_000_000);
}

#[test]
fn real_io_nests_overlapping_ops() {
    let flash = FlashInner::new_arc();

    flash.real_io_enter();
    flash.real_io_enter();
    let waiter = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_secs(10))))
    };
    while flash.timed_count() != 1 {
        thread::yield_now();
    }

    flash.real_io_exit();
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        flash.timed_count(),
        1,
        "the deadline must stay deferred while ANY real I/O op is still in flight"
    );

    let real_start = RealInstant::now();
    flash.real_io_exit();
    waiter.join().expect("waiter thread panicked");
    assert_fast(real_start);
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
/// all-under-the-`core`-lock protocol never underflows `active` (a `debug_assert` would
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
///     (`bracketed_on`) so the exit drops its `active` slot — proving no leak from
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
/// Every spawned body is `bracketed_on` (reset credit on entry, drop on exit)
/// the way the production spawn bracket does.
#[test]
fn stress_mixed_waits_no_underflow_no_lost_wakeup() {
    const ROUNDS: u64 = 16;
    const K: u64 = 5;
    const SPIN_WAITS: u64 = 3;

    let flash = FlashInner::new_arc();
    let real_start = RealInstant::now();

    for round in 0..ROUNDS {
        flash.reset();
        let coordinator = flash.test_hold();

        // Untimed waiters are released by the main thread after they register.
        // Collect their cvids to re-signal, and track how many have not yet woken
        // so the main thread keeps signalling until EVERY untimed worker has been
        // released. Polling `indef_count() == 0` alone is racy: a slow untimed
        // worker may not have called `register_condvar_untimed` when the loop
        // first observes an empty map, so it would exit before that worker parks
        // and then the worker would wait forever. The counter closes that window —
        // it starts non-zero and only the worker itself decrements it post-wake.
        let mut untimed_cvids: Vec<super::ids::CvId> = Vec::new();
        let untimed_left = Arc::new(AtomicUsize::new(0));

        // Busy-spin worker: NEVER waits. Must stay uncounted and never stall the
        // engine (no deadline, not in `active`). `bracketed_on` confirms exit is
        // a no-op when credit stayed `None`.
        let spin = {
            let flash = Arc::clone(&flash);
            thread::spawn(move || {
                bracketed_on(&flash, || {
                    for _ in 0..64 {
                        thread::yield_now();
                    }
                });
            })
        };

        // Decoder-build-like worker: N condvar waits with racing signals, exits
        // while `Running`. The bracket's `on_participant_exit` drops the slot.
        let builder = {
            let mut seed = round.wrapping_mul(2_654_435_761).wrapping_add(7);
            let flash = Arc::clone(&flash);
            thread::spawn(move || {
                bracketed_on(&flash, || {
                    for _ in 0..SPIN_WAITS {
                        let cvid = flash.next_condvar_id();
                        let deadline = flash.clock.now_nanos() + NANOS_PER_SEC;
                        let (token, adv, wait) = flash.register_condvar_timed(deadline, cvid);
                        let racer = {
                            let flash = Arc::clone(&flash);
                            thread::spawn(move || {
                                if xs(&mut seed) & 1 == 0 {
                                    thread::yield_now();
                                }
                                flash.signal_condvar(cvid, true);
                            })
                        };
                        adv.fire();
                        token.wait();
                        wait.resume();
                        racer.join().expect("builder signal racer panicked");
                    }
                    // Exit here while `Running`: the bracket must decrement.
                });
            })
        };

        // Scheduler-like worker: park/wake loop raced by `unpark`, then exits.
        let scheduler = {
            let mut seed = round.wrapping_mul(40_503).wrapping_add(13);
            let flash = Arc::clone(&flash);
            thread::spawn(move || {
                bracketed_on(&flash, || {
                    let me = super::ids::ThreadKey::of(thread::current().id());
                    for _ in 0..SPIN_WAITS {
                        let racer = {
                            let flash = Arc::clone(&flash);
                            thread::spawn(move || {
                                if xs(&mut seed) & 1 == 0 {
                                    thread::yield_now();
                                }
                                flash.unpark(me);
                            })
                        };
                        flash.park_timed_unparkable(Duration::from_secs(2), me);
                        racer.join().expect("scheduler unpark racer panicked");
                    }
                });
            })
        };

        let handles: Vec<_> = (0..K)
            .map(|idx| {
                let kind = (round.wrapping_mul(31).wrapping_add(idx)) % 3;
                let cvid = flash.next_condvar_id();
                if kind == 2 {
                    untimed_cvids.push(cvid);
                    untimed_left.fetch_add(1, Ordering::Relaxed);
                }
                let mut seed = round.wrapping_mul(1_000_003).wrapping_add(idx + 1);
                let left = Arc::clone(&untimed_left);
                let flash = Arc::clone(&flash);
                thread::spawn(move || {
                    bracketed_on(&flash, || match kind {
                        0 => {
                            // Thread park; a sibling racing unpark targets the
                            // SAME engine thread key. Either ordering is safe;
                            // the deadline guarantees return regardless.
                            let me = super::ids::ThreadKey::of(thread::current().id());
                            let racer = {
                                let flash = Arc::clone(&flash);
                                thread::spawn(move || {
                                    if xs(&mut seed) & 1 == 0 {
                                        thread::yield_now();
                                    }
                                    flash.unpark(me);
                                })
                            };
                            flash.park_timed_unparkable(Duration::from_secs(1 + (idx % 4)), me);
                            racer.join().expect("unpark racer panicked");
                        }
                        1 => {
                            // Timed condvar raced by a sibling signal.
                            let deadline =
                                flash.clock.now_nanos() + (1 + (idx % 4)) * NANOS_PER_SEC;
                            let (token, adv, wait) = flash.register_condvar_timed(deadline, cvid);
                            adv.fire();
                            let racer = {
                                let flash = Arc::clone(&flash);
                                thread::spawn(move || {
                                    if xs(&mut seed) & 1 == 0 {
                                        thread::yield_now();
                                    }
                                    flash.signal_condvar(cvid, true);
                                })
                            };
                            token.wait();
                            wait.resume();
                            racer.join().expect("signal racer panicked");
                        }
                        _ => {
                            // Untimed condvar: released by the main thread only.
                            let (token, adv, wait) = flash.register_condvar_untimed(cvid);
                            adv.fire();
                            token.wait();
                            wait.resume();
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
        drop(coordinator);
        while untimed_left.load(Ordering::Relaxed) != 0 {
            for cvid in &untimed_cvids {
                flash.signal_condvar(*cvid, true);
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
        assert_eq!(flash.active_count(), 0, "round {round}: active leaked");
        assert_eq!(flash.timed_count(), 0, "round {round}: timed waiter leaked");
        assert_eq!(flash.indef_count(), 0, "round {round}: indef waiter leaked");
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
    let flash = FlashInner::new_arc();
    let secs = [6_u64, 2, 9, 2, 4, 9, 1];
    let real_start = RealInstant::now();

    let run = || {
        flash.reset();
        let base = flash.clock.now_nanos();
        // The coordinator holds the clock at `base` until every worker has parked,
        // so each worker reads the same `base` for its absolute deadline (the
        // multiset is `base + s` for each `s`). Workers are `bracketed_on` so wake-
        // to-`Running`-then-exit balances `active` and the engine drains to zero.
        let coordinator = flash.test_hold();
        let handles: Vec<_> = secs
            .iter()
            .copied()
            .map(|s| {
                let flash = Arc::clone(&flash);
                thread::spawn(move || {
                    bracketed_on(&flash, || {
                        let cvid = flash.next_condvar_id();
                        let deadline = flash.clock.now_nanos() + s * NANOS_PER_SEC;
                        let (token, adv, wait) = flash.register_condvar_timed(deadline, cvid);
                        adv.fire();
                        token.wait();
                        wait.resume();
                    });
                })
            })
            .collect();
        while flash.timed_count() != secs.len() {
            thread::yield_now();
        }
        drop(coordinator);
        for h in handles {
            h.join().expect("waiter panicked");
        }
        (base, flash.clock.now_nanos(), flash.advance_log())
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

/// Regression for the flash(false) `yield_now` wedge (the `local_seek_middle_hang`
/// post-seek silence). With ambient OFF the surrounding task keeps its
/// `active_async` slot across the yield (its other primitives are REAL), so an
/// engine-backed yield could never be granted — its grant needs `active_async ==
/// 0`, a circular dependency the engine cannot break with no driver. `yield_now`
/// therefore branches on `flash_ambient`: a real scheduler yield when ambient is
/// off, so it resolves on the next poll WITHOUT the engine.
#[test]
fn ambient_off_yield_now_is_real_passthrough() {
    let _g = guard();
    reset();
    assert!(!flash_enabled());
    // No `ambient_scope(true)`: ambient is OFF (a flash(false) test / production).

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut task = Box::pin(participate(
        async {
            yield_now().await;
        },
        Location::caller(),
    ));

    // First poll yields (Pending after re-arming the waker); it touches NO engine
    // yield-waiter, so re-poll resolves it with no advance.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        sched::diag_yield_count(),
        0,
        "ambient-off yield must not register an engine yield-waiter"
    );
    assert!(
        task.as_mut().poll(&mut cx).is_ready(),
        "ambient-off yield must resolve on the next poll without an engine advance"
    );
}

/// Under ambient ON, `yield_now` stays engine-backed: it registers a yield-waiter
/// and parks (resolved only by an engine advance / the lone-yield rescue), so a
/// flash(true) busy-poll loop still relinquishes the virtual clock. This guards
/// against over-correcting the flash(false) fix into making yield real everywhere.
#[test]
fn ambient_on_yield_now_is_engine_backed() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut task = Box::pin(participate(
        async {
            yield_now().await;
        },
        Location::caller(),
    ));

    // First poll registers an engine yield-waiter. The participated task still
    // holds its `active_async` slot during the poll, so the lone-yield rescue does
    // NOT fire mid-poll; the waiter is granted on the gate-park quiescent edge.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    // The gate parked it (slot released), and the immediate advance from the park
    // granted the lone yield-waiter; the re-poll then resolves.
    assert!(task.as_mut().poll(&mut cx).is_ready());
}

/// An ambient `task::spawn_blocking` closure is REAL WORK IN FLIGHT: while it
/// runs (between engine parks) the virtual clock must not advance, or a parked
/// sibling sees `active == 0` and jumps its deadline against time the work never
/// had (the watchdog-burn failure mode). The closure spins until a real-time
/// helper releases it ~50ms later; a 10ms-virtual engine park taken meanwhile
/// must be held (measured in REAL elapsed) until the closure exits.
#[test]
fn ambient_blocking_closure_pins_virtual_clock() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let _rt = rt.enter();
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicUsize::new(0));
    let entered_in = Arc::clone(&entered);
    let release_in = Arc::clone(&release);
    let handle = crate::tokio::task::spawn_blocking(move || {
        entered_in.store(1, Ordering::Release);
        while release_in.load(Ordering::Acquire) == 0 {
            std::hint::spin_loop();
        }
    });
    while entered.load(Ordering::Acquire) == 0 {
        thread::yield_now();
    }
    let release_timer = Arc::clone(&release);
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        release_timer.store(1, Ordering::Release);
    });
    let start = RealInstant::now();
    sched::park_for(Duration::from_millis(10));
    let waited = start.elapsed();
    releaser.join().expect("releaser thread");
    rt.block_on(handle).expect("blocking closure joined");
    assert!(
        waited >= Duration::from_millis(40),
        "virtual clock advanced past a 10ms deadline while an ambient blocking \
         closure was still running (park returned after {waited:?} real)"
    );
}

/// `park_timeout`/`unpark` are a CROSS-THREAD pair, but each side resolves its
/// mode from its OWN thread flags — and those may disagree. Here the target has
/// no ambient (a raw pool thread, e.g. `tokio::task::spawn_blocking`) so its park
/// is a real OS park; the waker runs on a flash-ACTIVE callstack (the audio
/// worker's `run_loop`). The wake must land on the OS slot too, not vanish into
/// the engine's `unpark_pending` while the target sleeps out its full real
/// timeout. Delivery must hold in BOTH orderings (wake-then-park / park-then-wake),
/// so no synchronization beyond the handle send is needed.
#[cfg(not(feature = "loom"))]
#[test]
fn unpark_from_flash_callstack_reaches_real_parked_thread() {
    let _g = guard();
    reset();
    let start = RealInstant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let target = thread::spawn(move || {
        tx.send(thread::current()).expect("send park handle");
        crate::thread::park_timeout(Duration::from_secs(5));
    });
    let handle = rx.recv().expect("recv park handle");
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    crate::thread::unpark(&handle);
    target.join().expect("real-parked target panicked");
    assert_fast(start);
}

/// PIN — `park_for` resumes via the bare TLS `mark_running`, NOT
/// `WaitGuard::resume`. The harness `park_for` site is un-bracketed (no spawn
/// bracket balances its credit), so the non-dedicated `resume` arm would
/// wrongly settle the firer's wake bump (`active -= 1` + advance). The bump
/// must therefore still be visible after `park_for` returns; converting
/// `park_for` to `resume()` turns this red deterministically.
#[test]
fn park_for_keeps_firer_bump_unsettled() {
    let flash = FlashInner::new_arc();
    credit::reset_credit();
    flash.park_for(Duration::from_millis(3));
    assert_eq!(
        flash.active_count(),
        1,
        "park_for must resume via bare mark_running; a resume() conversion settles the bump to 0"
    );
    credit::reset_credit();
}
