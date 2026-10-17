use std::{
    future::Future,
    panic::Location,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    task::{Context, Wake, Waker},
    thread,
    time::Instant as RealInstant,
};

use kithara_test_utils::kithara;

use super::{
    Duration, FlashSleep, Instant, advance, ambient_scope, enter_dynamic, flash_enabled,
    join::{Join, JoinHandle},
    participate, reset,
    system::{FlashInner, credit, forward},
    time::{FlashTimeout, TimeoutError},
    yield_now,
};
use crate::{
    consts,
    sync::{Arc, Notify, ThreadGate, WaitGate},
};

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
    credit::mark_dedicated(Location::caller());
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
#[kithara::test(native, flash(false))]
fn no_block_bridged_engine_wait_panics() {
    #[cfg(feature = "no-block")]
    const NO_BLOCK_BRIDGED_BUDGET_MS: u64 = 10_000;

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
                crate::thread::park_timeout(Duration::from_millis(consts::NO_BLOCK_ENGINE_WAIT_MS));
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
#[kithara::test(native, flash(false))]
fn no_block_permit_poll_suppresses_bridged_wait_and_budget() {
    #[cfg(feature = "no-block")]
    const NO_BLOCK_TIGHT_BUDGET_MS: u64 = 1;

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
                crate::thread::park_timeout(Duration::from_millis(consts::NO_BLOCK_ENGINE_WAIT_MS));
            }),
            Location::caller(),
        ),
    );

    assert!(poll_once_no_block(task).is_ready());
}

#[kithara::test(native, flash(false))]
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

#[kithara::test(native, flash(false))]
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
#[kithara::test(native, flash(false))]
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
#[kithara::test(native, flash(false))]
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
    assert_eq!(forward::async_active_count(), 0, "parked task is uncounted");

    // Signal it. The task is now RUNNABLE (its waker fired, it is queued) but has
    // NOT been re-polled. It MUST be counted so a concurrent `try_advance` cannot
    notify.notify_one();
    assert_eq!(
        forward::async_active_count(),
        1,
        "a woken-but-not-yet-repolled task must keep the engine non-quiescent"
    );

    // Re-poll resolves it; the slot is released exactly once.
    assert!(task.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        forward::async_active_count(),
        0,
        "a completed task releases its slot exactly once"
    );
}

/// A pinning task's spawn site says WHERE it came from, never what it is doing.
/// A run's nine hangs were all pinned by one task at one spawn site, with
/// nothing in the dump to separate a task spinning through wake-poll-park from
/// one the runtime never re-polled after a wake. The poll count separates them.
#[kithara::test(native, flash(false))]
fn a_task_that_has_never_been_polled_pins_the_clock_at_zero_polls() {
    let _g = guard();
    reset();

    let notify = Notify::default();
    let _task = Box::pin(participate(
        async {
            notify.notified().await;
        },
        Location::caller(),
    ));

    let dump = forward::dump();
    assert!(dump.contains("polls=0"), "{dump}");
}

#[kithara::test(native, flash(false))]
fn a_pinning_task_reports_the_polls_it_entered() {
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

    // Park on the notify, then wake it: the task holds a slot again, now with
    // one entered poll behind it.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    notify.notify_one();

    let dump = forward::dump();
    assert!(dump.contains("polls=1"), "{dump}");
}

/// The stress report reads a wedge from these five names: it masks their
/// values as per-attempt noise so hangs cluster by shape, and what is left is
/// the shape itself. A rename here would silently shatter every hang into its
/// own cluster, so the header's vocabulary is pinned on this side of the
/// crate boundary.
#[kithara::test(native, flash(false))]
fn the_dump_header_names_every_way_an_advance_can_end() {
    let _g = guard();
    reset();

    let dump = forward::dump();
    for key in [
        "advances=",
        "advance_blocked=",
        "advance_no_deadline=",
        "advance_yield_releases=",
        "advance_paced_wait=",
    ] {
        assert!(dump.contains(key), "missing {key} in {dump}");
    }
}

/// The holder line is the dump's only lead on a wedged engine, and a spawned
/// job claims its credit on the CHILD thread: a `#[track_caller]` claim there
/// names the platform shim, so every blocking job in the process reads the
/// same and the dump cannot tell a decoder rebuild from a probe. The site is
/// therefore taken on the PARENT at spawn and carried in the reservation.
#[cfg(not(feature = "loom"))]
#[kithara::test(native, flash(false))]
fn a_spawned_holder_is_named_by_its_spawn_site_not_the_platform_shim() {
    let _g = guard();
    reset();

    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = crate::thread::spawn_named("dump-holder", move || {
        claimed_tx.send(()).expect("announce the claim");
        release_rx.recv().expect("await release");
    });
    claimed_rx.recv().expect("the holder must claim its credit");

    let dump = forward::dump();
    release_tx.send(()).expect("release the holder");
    holder.join().expect("holder panicked");

    assert!(
        dump.contains(&format!("resumed_from={}:", file!())),
        "the holder must be named by its spawn site, not by the shim\n{dump}"
    );
}

/// The holder line's age is the dump's only measure of how long the engine has
/// been pinned, and it used to be read off the VIRTUAL clock — the clock that
/// holder is stopping. It therefore printed 0 in exactly the case it exists
/// for: every wedge dump of the 2026-09-26 stress run said the pin was 0 ns
/// old after minutes of real time. It is aged on the real clock instead.
#[cfg(not(feature = "loom"))]
#[kithara::test(native, flash(false))]
fn a_pinning_holder_is_aged_on_the_real_clock_not_the_virtual_one_it_stops() {
    let flash = FlashInner::new_arc();
    flash.sync_holder_running(Location::caller());
    flash.clock.advance(5 * consts::NANOS_PER_SEC);

    let dump = flash.to_string();
    let age: u64 = dump
        .split("held_for_real_ns=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("the dump must age its active holder");
    assert!(
        age < consts::NANOS_PER_SEC,
        "the pin's age must not follow the virtual clock it stops: {age} ns\n{dump}"
    );
}

/// A dump lists EVERY parked waiter and says nothing about which one the clock
/// is waiting on: the four hangs of run #11 each had one deadline-less
/// waiter holding it, and the pin had to be re-derived by hand from the
/// counters. A deadline-less waiter is freed by nothing but a matching signal,
/// so when its task is still counted, that waiter IS the pin — mark it.
#[kithara::test(native, flash(false))]
fn a_deadline_less_waiter_of_a_counted_task_is_marked_as_pinning() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);

    let notify = Notify::default();
    let dumped = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&dumped);
    let mut task = Box::pin(participate(
        async move {
            // Park on the deadline-less waiter WITHOUT ending this poll, so the
            // snapshot catches the task still counted — the shape every real
            // hang dump showed: `state=Running` plus an `indef` entry naming
            // that same task.
            let mut wait = Box::pin(notify.notified());
            let inner = Waker::from(Arc::new(NoopWake));
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&inner))
                    .is_pending(),
                "a fresh notify must park its first poll"
            );
            *seen.lock().unwrap_or_else(PoisonError::into_inner) = forward::dump();
        },
        Location::caller(),
    ));
    assert!(
        task.as_mut().poll(&mut cx).is_ready(),
        "the body runs to completion in one poll: it never awaits"
    );

    let dump = dumped.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(dump.contains("pins_clock"), "{dump}");
}

/// The other half of that contract. A healthy flash test parks hundreds of
/// deadline-less waiters whose tasks are simply idle; marking those would make
/// the pin marker — and the parking stack it gates — noise again.
#[kithara::test(native, flash(false))]
fn a_deadline_less_waiter_of_a_parked_task_is_not_marked_as_pinning() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);

    let notify = Notify::default();
    let mut task = Box::pin(participate(
        async move {
            notify.notified().await;
        },
        Location::caller(),
    ));
    assert!(
        task.as_mut().poll(&mut cx).is_pending(),
        "the task must park on the notify"
    );

    let dump = forward::dump();
    assert!(
        dump.contains("indef id="),
        "precondition: the parked waiter must be in the dump at all\n{dump}"
    );
    assert!(!dump.contains("pins_clock"), "{dump}");
}

/// A task left `RUNNABLE` by a wake keeps its slot until it is re-polled — but
/// on a `current_thread` runtime the only thread that can re-poll it is the
/// `block_on` thread, and a synchronous engine wait taken INSIDE a poll (a
/// render loop's deliberate pace) blocks exactly that thread. The bridged branch
/// releases the BLOCKING task's slot, not the stranded one's, so the stranded
/// task pins the clock, the pace deadline never fires, and the poll that would
/// clear the pin never runs — the crossfade hang, whose sixteen dumps all named
/// one `Runnable` task with a frozen poll count. A task no live thread can poll
/// must not pin the clock. Broken: this hangs forever (the deadlock verbatim).
#[kithara::test(native, flash(false))]
fn a_runnable_task_whose_only_poller_is_bridged_does_not_pin_the_clock() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    // What makes the driver the task's ONLY poller is the runtime flavor;
    // entering a current-thread runtime is how the production case
    // (`#[kithara::test(tokio)]` without `multi_thread`) declares it.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let _rt = rt.enter();
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);

    let notify = Notify::default();
    let mut stranded = Box::pin(participate(
        async {
            notify.notified().await;
        },
        Location::caller(),
    ));
    // Park it, then wake it: `Runnable` with the slot held, and this thread —
    // its only possible poller — is about to block instead of re-polling it.
    assert!(stranded.as_mut().poll(&mut cx).is_pending());
    notify.notify_one();
    assert_eq!(
        forward::async_active_count(),
        1,
        "a woken-but-not-yet-repolled task must hold its slot"
    );

    let start = RealInstant::now();
    let mut driver = Box::pin(participate(
        async {
            crate::thread::sleep(Duration::from_millis(10));
        },
        Location::caller(),
    ));
    assert!(
        driver.as_mut().poll(&mut cx).is_ready(),
        "the bridged wait must be granted: nothing else can poll the task pinning it"
    );
    assert_fast(start);
}

#[kithara::test(native, flash(false))]
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

#[kithara::test(native, flash(false))]
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

/// The two arithmetic tests below are pure `Instant` math: no ambient, so
/// `Instant::now()` is the REAL arm (a lock-free monotonic read off the
/// process clock anchor) and no engine state — global or local — is touched.
/// Instance-agnostic, hence no GUARD, no reset, no local engine.
#[kithara::test(native, flash(false))]
fn arithmetic_saturates_and_orders() {
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    let earlier = now - Duration::from_secs(1);
    assert!(earlier < now && now < later);
    assert_eq!(later - now, Duration::from_secs(1));
    assert_eq!(later.duration_since(now), Duration::from_secs(1));
    assert_eq!(now.saturating_duration_since(later), Duration::ZERO);
}

#[kithara::test(native, flash(false))]
fn base_keeps_backward_offset_positive() {
    let now = Instant::now();
    let earlier = now - Duration::from_secs(3600);
    assert_eq!(now.duration_since(earlier), Duration::from_secs(3600));
}

/// A timeout dates its deadline from the clock the engine reads when the
/// deadline is REGISTERED, and the work it guards registers waits of its own.
/// Arming after that work runs would therefore date the deadline from a clock
/// the work had already moved, and a 100ms timeout over a 150ms wait would land
/// at 250ms - the longer inner wait outliving the shorter timeout. Driven by a
/// bare manual poll, which pins nothing, so every registration advances the
/// clock at once: the same mid-poll advance a paced `flash(io)` region allows.
#[kithara::test(native, flash(false))]
fn a_timeout_arms_its_deadline_before_the_work_it_guards_moves_the_clock() {
    let _g = guard();
    reset();
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut guarded = Box::pin(FlashTimeout {
        future: FlashSleep::new(Duration::from_millis(150)),
        sleep: FlashSleep::new(Duration::from_millis(100)),
    });

    assert!(
        matches!(
            guarded.as_mut().poll(&mut cx),
            std::task::Poll::Ready(Err(TimeoutError))
        ),
        "a 100ms timeout must expire on work that waits 150ms"
    );
}

#[kithara::test(native, flash(false))]
fn quiescence_advances_to_max_deadline_fast() {
    let flash = FlashInner::new_arc();
    let base = flash.clock.now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters_on(&flash, &[10, 3]);

    assert_fast(real_start);
    assert_eq!(
        flash.clock.now_nanos(),
        base + 10 * consts::NANOS_PER_SEC,
        "clock lands on the max registered deadline"
    );
    assert_eq!(
        flash.advance_log(),
        vec![
            base + 3 * consts::NANOS_PER_SEC,
            base + 10 * consts::NANOS_PER_SEC
        ],
        "advance jumps to the min deadline first, then the next, regardless of start order"
    );
}

#[kithara::test(native, flash(false))]
fn equal_deadlines_wake_in_one_step() {
    let flash = FlashInner::new_arc();
    let base = flash.clock.now_nanos();
    let real_start = RealInstant::now();

    run_parked_waiters_on(&flash, &[5, 5, 8]);

    assert_fast(real_start);
    assert_eq!(
        flash.advance_log(),
        vec![
            base + 5 * consts::NANOS_PER_SEC,
            base + 8 * consts::NANOS_PER_SEC
        ],
        "three waiters, two advance steps: the two equal deadlines wake in one batch"
    );
    assert_eq!(flash.clock.now_nanos(), base + 8 * consts::NANOS_PER_SEC);
}

#[kithara::test(native, flash(false))]
fn deferred_worker_poll_precedes_distant_timer() {
    let _g = guard();
    let _a = ambient_scope(true);

    assert_eq!(
        run_deferred_worker_poll(false),
        3_000,
        "an ordinary gate backstop must not drive the clock ahead of the distant timer"
    );
    assert_eq!(
        run_deferred_worker_poll(true),
        10,
        "deferred work must be polled at the worker's 10ms interval, before the 3s timer"
    );
}

fn run_deferred_worker_poll(poll_deadline: bool) -> usize {
    reset();
    let flash = Arc::clone(&super::system::FLASH);
    let base = flash.clock.now_nanos();
    let gate = Arc::new(ThreadGate::default());
    let deferred = Arc::new(AtomicUsize::new(0));
    let observed_ms = Arc::new(AtomicUsize::new(0));
    let coordinator = flash.test_hold();

    let worker = {
        let flash = Arc::clone(&flash);
        let gate = Arc::clone(&gate);
        let deferred = Arc::clone(&deferred);
        let observed_ms = Arc::clone(&observed_ms);
        thread::spawn(move || {
            bracketed_on(&flash, || {
                let since = gate.current();
                let signalled = if poll_deadline {
                    gate.wait_poll_timeout(since, Duration::from_millis(10))
                } else {
                    gate.wait_timeout(since, Duration::from_millis(10))
                };
                assert!(!signalled);
                assert_eq!(deferred.load(Ordering::Acquire), 1);
                observed_ms.store(
                    ((flash.clock.now_nanos() - base) / 1_000_000) as usize,
                    Ordering::Release,
                );
            });
        })
    };
    let producer = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || {
            bracketed_on(&flash, || flash.park_for(Duration::from_secs(3)));
        })
    };
    let notifier = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || {
            bracketed_on(&flash, || {
                flash.park_for(Duration::from_millis(5));
                deferred.store(1, Ordering::Release);
            });
        })
    };
    while flash.timed_count() != 3 {
        thread::yield_now();
    }
    drop(coordinator);

    notifier.join().expect("deferred notifier panicked");
    worker.join().expect("worker panicked");
    producer.join().expect("distant timer panicked");
    assert_eq!(flash.active_count(), 0, "all wake credits must settle");
    observed_ms.load(Ordering::Acquire)
}

#[kithara::test(native, flash(false))]
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
            base_a + 2 * consts::NANOS_PER_SEC,
            base_a + 4 * consts::NANOS_PER_SEC,
            base_a + 7 * consts::NANOS_PER_SEC,
            base_a + 11 * consts::NANOS_PER_SEC,
        ],
        "distinct deadlines advance in sorted order"
    );
}

#[kithara::test(native, flash(false))]
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
        base + 4 * consts::NANOS_PER_SEC,
        "first-park bootstrap self-advanced the lone waiter"
    );
    assert_eq!(flash.advance_log(), vec![base + 4 * consts::NANOS_PER_SEC]);
    assert_eq!(flash.active_count(), 0, "bootstrap balanced on exit");
}

#[kithara::test(native, flash(false))]
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
        t0 + 10 * consts::NANOS_PER_SEC,
        "full-speed collapse resumes the instant the last real I/O op completes"
    );
}

#[kithara::test(native, flash(false))]
fn real_io_paces_deadline_to_real_time_not_pin() {
    let flash = FlashInner::new_arc();
    let t0 = flash.clock.now_nanos();

    flash.real_io_enter();
    let real_start = RealInstant::now();
    let waiter = {
        let flash = Arc::clone(&flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(Duration::from_millis(20))))
    };

    // Pace, not pin: a virtually delayed peer progresses while the client holds real I/O.
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

#[kithara::test(native, flash(false))]
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
#[kithara::test(native, flash(false))]
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
                        let deadline = flash.clock.now_nanos() + consts::NANOS_PER_SEC;
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
                        flash.park_timed_unparkable(
                            Duration::from_secs(2),
                            me,
                            super::system::ParkRole::Deadline,
                        );
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
                            flash.park_timed_unparkable(
                                Duration::from_secs(1 + (idx % 4)),
                                me,
                                super::system::ParkRole::Deadline,
                            );
                            racer.join().expect("unpark racer panicked");
                        }
                        1 => {
                            // Timed condvar raced by a sibling signal.
                            let deadline =
                                flash.clock.now_nanos() + (1 + (idx % 4)) * consts::NANOS_PER_SEC;
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
#[kithara::test(native, flash(false))]
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
                        let deadline = flash.clock.now_nanos() + s * consts::NANOS_PER_SEC;
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
            base_a + consts::NANOS_PER_SEC,
            base_a + 2 * consts::NANOS_PER_SEC,
            base_a + 4 * consts::NANOS_PER_SEC,
            base_a + 6 * consts::NANOS_PER_SEC,
            base_a + 9 * consts::NANOS_PER_SEC,
        ],
        "min-jump over the deadline multiset",
    );
    assert_eq!(
        end_a,
        base_a + 9 * consts::NANOS_PER_SEC,
        "clock lands on max deadline"
    );
    assert_eq!(end_b, base_b + 9 * consts::NANOS_PER_SEC);
}

/// Regression for the flash(false) `yield_now` wedge (the `local_seek_middle_hang`
/// post-seek silence). With ambient OFF the surrounding task keeps its
/// `active_async` slot across the yield (its other primitives are REAL), so an
/// engine-backed yield could never be granted — its grant needs `active_async ==
/// 0`, a circular dependency the engine cannot break with no driver. `yield_now`
/// therefore branches on `flash_ambient`: a real scheduler yield when ambient is
/// off, so it resolves on the next poll WITHOUT the engine.
#[kithara::test(native, flash(false))]
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
        forward::diag_yield_count(),
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
#[kithara::test(native, flash(false))]
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

#[kithara::test(native, flash(false))]
fn a_granted_yield_keeps_credit_until_consumed_or_cancelled() {
    let _guard = guard();
    reset();
    let _ambient = ambient_scope(true);
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);

    for cancelled in [true, false] {
        let hold = super::system::FLASH.test_hold();
        let mut yielded = Box::pin(yield_now());
        assert!(yielded.as_mut().poll(&mut context).is_pending());
        drop(hold);
        assert_eq!(super::system::FLASH.active_count(), 1);
        if !cancelled {
            assert!(yielded.as_mut().poll(&mut context).is_ready());
        }
        drop(yielded);
        assert_eq!(super::system::FLASH.active_count(), 0);
    }
}

/// A yielder is a RUNNABLE participant, so every turn it asks for is handed to
/// it. Rationing them — one per virtual instant, say — strands any task that
/// yields more than once before its first deadline, and sends the clock to the
/// farthest park it can find while that work is still in flight. Budgets on
/// that path then expire on the virtual clock having spent no real seconds,
/// and the hang detectors fire on a stall that never happened.
#[kithara::test(native, flash(false))]
fn repeated_yields_are_each_granted_never_rationed() {
    const YIELDS: usize = 4;

    let _g = guard();
    reset();
    let _a = ambient_scope(true);

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut task = Box::pin(participate(
        async {
            for _ in 0..YIELDS {
                yield_now().await;
            }
        },
        Location::caller(),
    ));

    // Each yield costs one poll to register and is granted on the gate-park
    // edge that ends it, so the body needs exactly one poll more than it yields.
    let mut ready_at = None;
    for poll in 1..=YIELDS + 1 {
        if task.as_mut().poll(&mut cx).is_ready() {
            ready_at = Some(poll);
            break;
        }
    }
    assert_eq!(
        ready_at,
        Some(YIELDS + 1),
        "a yield turn was rationed instead of granted"
    );
}

/// An ambient `task::spawn_blocking` closure is REAL WORK IN FLIGHT: while it
/// runs (between engine parks) the virtual clock must not advance, or a parked
/// sibling sees `active == 0` and jumps its deadline against time the work never
/// had (the watchdog-burn failure mode). The closure spins until a real-time
/// helper releases it ~50ms later; a 10ms-virtual engine park taken meanwhile
/// must be held (measured in REAL elapsed) until the closure exits.
#[kithara::test(native, flash(false))]
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
    forward::park_for(Duration::from_millis(10));
    let waited = start.elapsed();
    releaser.join().expect("releaser thread");
    rt.block_on(handle).expect("blocking closure joined");
    assert!(
        waited >= Duration::from_millis(40),
        "virtual clock advanced past a 10ms deadline while an ambient blocking \
         closure was still running (park returned after {waited:?} real)"
    );
}

/// The other half of what a pooled closure declares: `spin_loop` is work, a
/// cooperative yield is the absence of it, and only the first may pin the
/// clock. Both reach the engine as one dedicated credit, so until the yield
/// told the engine apart from the work, the one thread able to freeze the
/// clock held the one yield unable to thaw it — a spin that outlived every
/// virtual deadline around it, which is how
/// `test_seek_complete_emitted_only_after_output_commit[chunk]` reached an
/// outer kill with its holder still on CPU.
///
/// Mirrors [`ambient_blocking_closure_pins_virtual_clock`]: same shape, same
/// 50ms real release, opposite verdict on the sibling's 10ms virtual park.
#[kithara::test(native, flash(false))]
fn a_yielding_blocking_closure_releases_the_virtual_clock() {
    let waited = park_beside_a_waiting_closure(crate::thread::yield_now, park_for_10ms);
    assert!(
        waited < Duration::from_millis(40),
        "a yielding blocking closure held the virtual clock for its whole real \
         lifetime: a 10ms deadline took {waited:?} real to fire"
    );
}

/// The backoff twin of [`a_yielding_blocking_closure_releases_the_virtual_clock`].
/// Async code backs off through `spawn_blocking(|| paced_backoff(..))`, and on the
/// pooled thread that backoff was a real sleep under the closure's credit. Each
/// step held the clock still, so a poll loop that backs off until a timed
/// producer delivers kept the producer's own deadlines from firing. That is how
/// `packaged_abr_switch_keeps_player_continuity` reached its 30s wall timeout
/// with a quarter of a virtual second spent.
#[kithara::test(native, flash(false))]
fn a_backing_off_blocking_closure_releases_the_virtual_clock() {
    let waited = park_beside_a_waiting_closure(
        || crate::thread::paced_backoff(Duration::from_millis(1)),
        park_for_10ms,
    );
    assert!(
        waited < Duration::from_millis(40),
        "a backing-off blocking closure held the virtual clock for its whole real \
         lifetime: a 10ms deadline took {waited:?} real to fire"
    );
}

/// A gate poll is a deadline the clock owes a stop, not a backstop under an
/// edge, even though it parks a thread. A yielder beside it used to be turned
/// back at the current instant whenever every other park was a thread park,
/// so a loop that backed off until a backpressured worker's poll moved its
/// data froze the clock short of that poll. That is how
/// `packaged_abr_switch_keeps_player_continuity` spun at 1.07 virtual seconds
/// through millions of yields.
#[kithara::test(native, flash(false))]
fn a_yielding_closure_lets_the_clock_reach_a_gate_poll() {
    let waited = park_beside_a_waiting_closure(crate::thread::yield_now, || {
        let gate = ThreadGate::default();
        assert!(!gate.wait_poll_timeout(gate.current(), Duration::from_millis(10)));
    });
    assert!(
        waited < Duration::from_millis(40),
        "a yielding blocking closure kept the clock short of a 10ms gate poll, \
         which took {waited:?} real to fire"
    );
}

/// A runnable yield is the opposite promise: the closure still has work, so it
/// pins the clock as a spinning closure does
/// ([`ambient_blocking_closure_pins_virtual_clock`]) and the sibling's 10ms
/// park waits for the closure to finish.
#[kithara::test(native, flash(false))]
fn a_runnable_yield_holds_the_virtual_clock() {
    let waited = park_beside_a_waiting_closure(crate::thread::yield_runnable, park_for_10ms);
    assert!(
        waited >= Duration::from_millis(40),
        "the clock advanced past a closure that yielded with work in hand: a 10ms \
         deadline fired after {waited:?} real"
    );
}

/// The async twin of [`a_runnable_yield_holds_the_virtual_clock`]. A task that
/// yields with work in hand stays counted across the turn, so a deadline
/// registered before the yields does not come due. A yield that claimed the
/// task had nothing to do sent the clock to the next deadline, past work
/// already queued behind it: a stored `Notify` permit for a segment the reader
/// was waiting on, skipped by a 30 s ABR re-tick.
#[kithara::test(native, flash(false))]
fn a_runnable_task_yield_holds_the_virtual_clock() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let ((), took) = on_virtual_clock(async {
        let mut deadline = std::pin::pin!(crate::time::sleep(Duration::from_secs(1)));
        std::future::poll_fn(|cx| {
            assert!(deadline.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        for _ in 0..4 {
            crate::tokio::task::yield_runnable().await;
        }
    });
    assert_eq!(
        took,
        Duration::ZERO,
        "a runnable yield let the clock advance to a pending deadline"
    );
}

fn park_for_10ms() {
    forward::park_for(Duration::from_millis(10));
}

/// Run an ambient `spawn_blocking` closure that loops on `wait` until a helper
/// releases it ~50ms real later, and return the real time a 10ms virtual engine
/// `park` took meanwhile.
fn park_beside_a_waiting_closure(wait: fn(), park: fn()) -> Duration {
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
            wait();
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
    park();
    let waited = start.elapsed();
    releaser.join().expect("releaser thread");
    rt.block_on(handle).expect("blocking closure joined");
    waited
}

/// Run `body` as the root task of a current-thread runtime, under flash, and
/// return its output with the virtual time it took.
fn on_virtual_clock<F: Future>(body: F) -> (F::Output, Duration) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let start = Instant::now();
    let out = rt.block_on(participate(body, Location::caller()));
    (out, start.elapsed())
}

/// The runtime wakes a joiner only after the joined work has returned, and
/// the work used to give up its engine slot as it returned. In between, every
/// participant looked parked, so the clock jumped to the joiner's own deadline
/// before the joiner learned the work was done. That is how a cover read that
/// took no virtual time missed a two-second wait in
/// `only_the_current_attempt_places_its_cover`. The closure returns only
/// once its joiner has parked on it, which opens exactly that window.
#[kithara::test(native, flash(false))]
fn a_joined_blocking_closure_holds_the_clock_until_its_joiner_wakes() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let (joined, took) = on_virtual_clock(async {
        let handle = crate::tokio::task::spawn_blocking(|| {
            while forward::async_active_count() != 0 {
                thread::yield_now();
            }
        });
        crate::time::timeout(Duration::from_secs(2), handle).await
    });
    assert!(matches!(joined, Ok(Ok(()))), "{joined:?}");
    assert_eq!(
        took,
        Duration::ZERO,
        "the clock moved between the closure's return and its joiner's wake"
    );
}

/// The same window for an async task: on a current-thread runtime the task
/// first runs once its joiner has parked on it, and it finishes in that poll.
#[kithara::test(native, flash(false))]
fn a_joined_task_holds_the_clock_until_its_joiner_wakes() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let (joined, took) = on_virtual_clock(async {
        let handle = crate::tokio::task::spawn(async {});
        crate::time::timeout(Duration::from_secs(2), handle).await
    });
    assert!(matches!(joined, Ok(Ok(()))), "{joined:?}");
    assert_eq!(
        took,
        Duration::ZERO,
        "the clock moved between the task's completion and its joiner's wake"
    );
}

/// The other half: work nobody is waiting on has no joiner to hold its slot
/// for, so it gives the slot up as it finishes. Holding it until the handle is
/// next polled would freeze the clock under a joiner that sleeps first.
#[kithara::test(native, flash(false))]
fn an_unjoined_task_lets_the_clock_advance_when_it_finishes() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let (joined, took) = on_virtual_clock(async {
        let handle = crate::tokio::task::spawn(async {});
        crate::time::sleep(Duration::from_secs(1)).await;
        handle.await
    });
    assert!(joined.is_ok(), "{joined:?}");
    assert_eq!(took, Duration::from_secs(1));
}

/// A poll of the handle that the task budget turns away registers no join
/// wake, and a runtime that stops running drops the wake it deferred. A slot
/// held for that poll would then have nothing to release it. Here the root
/// task spends its budget, polls the handle once and returns while the closure
/// is still running; the closure finishes with nobody waiting on it, and the
/// clock must stay free.
#[kithara::test(native, flash(false))]
fn a_join_poll_the_budget_turns_away_holds_no_slot() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let release = Arc::new(AtomicUsize::new(0));
    let release_in = Arc::clone(&release);
    let body = async move {
        let mut handle = crate::tokio::task::spawn_blocking(move || {
            while release_in.load(Ordering::Acquire) == 0 {
                crate::thread::yield_now();
            }
        });
        std::future::poll_fn(|cx| {
            while let std::task::Poll::Ready(spent) = tokio::task::coop::poll_proceed(cx) {
                spent.made_progress();
            }
            assert!(std::pin::Pin::new(&mut handle).poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        handle
    };
    let handle = rt.block_on(participate(body, Location::caller()));
    release.store(1, Ordering::Release);
    while !handle.is_finished() {
        thread::yield_now();
    }
    let start = Instant::now();
    rt.block_on(participate(
        crate::time::sleep(Duration::from_secs(1)),
        Location::caller(),
    ));
    assert_eq!(start.elapsed(), Duration::from_secs(1));
}

/// A starved poll loop must not buy virtual time with its own backoff. A dated
/// backoff registers a free `Timed` deadline that the engine services in
/// isolation: each wake re-polls and re-sleeps, so a consumer whose producer is
/// waiting on real work walks the clock forward by itself until it has expired
/// the producer's own deadlines - the `phase_continuity` wall timeout, where an
/// async pull raced the clock 1060 virtual seconds inside a 25s budget while
/// every producer sat parked. Routed through `spawn_blocking` the same backoff
/// is an undated engine yield: with no deadline to reach, it leaves the clock
/// where it found it. Distinct from
/// `a_backing_off_blocking_closure_releases_the_virtual_clock`, which pins the
/// other half - that the backoff lets a sibling's deadline fire.
#[kithara::test(native, flash(false))]
fn a_starved_backoff_loop_does_not_advance_the_virtual_clock() {
    const STARVED_BACKOFF_STEP_MS: u64 = 1;
    const STARVED_BACKOFF_RETRIES: usize = 16;

    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let _rt = rt.enter();
    let _f = enter_dynamic(true);
    let t0 = Instant::now();
    rt.block_on(async {
        for _ in 0..STARVED_BACKOFF_RETRIES {
            crate::tokio::task::spawn_blocking(|| {
                crate::thread::paced_backoff(Duration::from_millis(STARVED_BACKOFF_STEP_MS))
            })
            .await
            .expect("backoff closure joined");
        }
    });
    assert_eq!(
        Instant::now().duration_since(t0),
        Duration::ZERO,
        "{STARVED_BACKOFF_RETRIES} starved retries moved the virtual clock: the backoff dated \
         its own wakes instead of spending real time"
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
#[kithara::test(native, flash(false))]
fn unpark_from_flash_callstack_reaches_real_parked_thread() {
    let _g = guard();
    reset();
    let start = RealInstant::now();
    let (tx, rx) = mpsc::channel();
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
#[kithara::test(native, flash(false))]
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

fn assert_spawn_clock_propagation(on_handle: bool) {
    let _g = guard();
    reset();
    let start = RealInstant::now();
    let parent_thread = thread::current().id();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("build clean worker runtime");
    let _rt = rt.enter();
    let frozen = {
        let _a = ambient_scope(true);
        let _f = enter_dynamic(true);
        advance(Duration::from_secs(3_600));
        Instant::now()
    };

    for (ambient, active) in [(true, true), (true, false), (false, true)] {
        let expected_active = ambient && active;
        let (sampled, sample) = mpsc::channel();
        let (release, resume) = tokio::sync::oneshot::channel();
        let future = async move {
            assert_ne!(thread::current().id(), parent_thread);
            let sample = || (Instant::now(), flash_enabled(), super::ambient_snapshot());
            sampled.send(sample()).expect("report first worker poll");
            resume.await.expect("resume worker task");
            sample()
        };
        let child = {
            let _a = ambient_scope(ambient);
            let _f = enter_dynamic(active);
            if on_handle {
                crate::tokio::task::spawn_on(rt.handle(), future)
            } else {
                crate::tokio::task::spawn(future)
            }
        };
        assert!(!flash_enabled());
        assert!(!super::ambient_snapshot());
        let check_sample = |sample: (Instant, bool, bool)| {
            if expected_active {
                assert_eq!(sample.0, frozen, "spawned task lost the virtual clock");
            } else {
                assert!(sample.0 < frozen, "a real carve must keep the real clock");
            }
            assert_eq!(sample.1, expected_active);
            assert_eq!(sample.2, ambient);
        };
        check_sample(sample.recv().expect("observe first worker poll"));
        let restored = rt
            .block_on(rt.spawn(async { (flash_enabled(), super::ambient_snapshot()) }))
            .expect("observe worker after pending poll");
        assert_eq!(restored, (false, false));
        release.send(()).expect("release worker task");
        check_sample(rt.block_on(child).expect("join worker task"));
        let restored = rt
            .block_on(rt.spawn(async { (flash_enabled(), super::ambient_snapshot()) }))
            .expect("observe worker after ready poll");
        assert_eq!(restored, (false, false));
    }
    assert_fast(start);
}

#[kithara::test(native, flash(false))]
fn spawned_async_task_inherits_dynamic_clock() {
    assert_spawn_clock_propagation(false);
}

#[kithara::test(native, flash(false))]
fn handle_spawned_async_task_inherits_dynamic_clock() {
    assert_spawn_clock_propagation(true);
}

#[kithara::test(native, flash(false))]
fn required_worker_poll_is_not_starved_by_repeated_yields() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let flash = Arc::clone(&super::system::FLASH);
    let base = flash.clock.now_nanos();
    let gate = Arc::new(ThreadGate::default());
    let deferred = Arc::new(AtomicUsize::new(0));
    let observed_ms = Arc::new(AtomicUsize::new(0));
    let coordinator = flash.test_hold();
    let real_start = RealInstant::now();

    let worker = {
        let flash = Arc::clone(&flash);
        let gate = Arc::clone(&gate);
        let deferred = Arc::clone(&deferred);
        let observed_ms = Arc::clone(&observed_ms);
        thread::spawn(move || {
            bracketed_on(&flash, || {
                let since = gate.current();
                assert!(!gate.wait_poll_timeout(since, Duration::from_millis(10)));
                assert_eq!(
                    deferred.swap(0, Ordering::AcqRel),
                    1,
                    "the required poll must consume work queued without a signal"
                );
                observed_ms.store(
                    ((flash.clock.now_nanos() - base) / 1_000_000) as usize,
                    Ordering::Release,
                );
            });
        })
    };
    while flash.timed_count() != 1 {
        thread::yield_now();
    }
    deferred.store(1, Ordering::Release);

    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut task = Box::pin(participate(
        async {
            for _ in 0..4 {
                yield_now().await;
            }
        },
        Location::caller(),
    ));
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert_eq!(forward::diag_yield_count(), 1);
    drop(coordinator);
    let first_yield_ms = (flash.clock.now_nanos() - base) / 1_000_000;
    while task.as_mut().poll(&mut cx).is_pending() {
        thread::yield_now();
    }
    drop(task);
    worker.join().expect("required poll worker panicked");

    assert_fast(real_start);
    assert_eq!(observed_ms.load(Ordering::Acquire), 10);
    assert_eq!(deferred.load(Ordering::Acquire), 0);
    assert_eq!(flash.active_count(), 0, "sync wake credits must settle");
    assert_eq!(
        forward::async_active_count(),
        0,
        "async wake credits must settle"
    );
    assert_eq!(flash.timed_count(), 0, "thread deadline must settle");
    assert_eq!(forward::diag_yield_count(), 0, "every yield must settle");
    assert_eq!(
        first_yield_ms, 10,
        "the required thread deadline must be served alongside the first pending yield"
    );
}

/// The pooled body can finish before Tokio publishes its result. A pending join
/// must keep the clock still across that gap, including unwind; native wakes that
/// only yield cooperative budget must not settle the handoff early.
#[kithara::test(native, flash(false))]
fn blocking_join_holds_credit_until_native_result_publication() {
    struct StagedBlocking {
        handle: JoinHandle<u8>,
        completion: Arc<Join>,
        run: mpsc::Sender<()>,
        exited: mpsc::Receiver<()>,
        publish: mpsc::Sender<()>,
    }

    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .expect("build controlled blocking runtime");
    let _rt = rt.enter();
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let staged = |panics: bool| {
        let (run, run_rx) = mpsc::channel();
        let (exited_tx, exited) = mpsc::channel();
        let (publish, publish_rx) = mpsc::channel();
        let completion = Join::new(true);
        let work = super::thread::joined_pool_task(
            move || {
                run_rx.recv().expect("release pooled work");
                assert!(!panics, "controlled blocking panic");
                7_u8
            },
            Some(Arc::clone(&completion)),
        );
        let native = tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            exited_tx.send(()).expect("report pooled exit");
            publish_rx
                .recv()
                .expect("release native result publication");
            match result {
                Ok(value) => value,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        });
        let handle = JoinHandle::new(native, completion.clone());
        StagedBlocking {
            handle,
            completion,
            run,
            exited,
            publish,
        }
    };

    for panics in [false, true] {
        let StagedBlocking {
            handle,
            completion,
            run,
            exited,
            publish,
        } = staged(panics);
        let abort = handle.abort_handle();
        assert_eq!(handle.id(), abort.id());
        assert!(!handle.is_finished());
        let start = Instant::now();
        let mut waiting = Box::pin(participate(
            FlashTimeout {
                future: handle,
                sleep: FlashSleep::new(Duration::from_secs(2)),
            },
            Location::caller(),
        ));
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        Wake::wake_by_ref(&completion);
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        run.send(()).expect("let pooled work finish");
        exited.recv().expect("pooled wrapper exited");
        assert_eq!(
            Instant::now().duration_since(start),
            Duration::ZERO,
            "pooled exit advanced the deadline before native result publication"
        );
        Wake::wake_by_ref(&completion);
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            Instant::now().duration_since(start),
            Duration::ZERO,
            "a nonterminal native wake settled completion credit"
        );
        assert!(!abort.is_finished(), "native publication remains held");
        publish.send(()).expect("publish native result");
        while super::system::FLASH.active_count() != 0 {
            thread::yield_now();
        }
        let result = loop {
            if let std::task::Poll::Ready(result) = waiting.as_mut().poll(&mut cx) {
                break result.expect("virtual deadline must not expire");
            }
            thread::yield_now();
        };
        if panics {
            assert!(
                result
                    .expect_err("native panic remains a join error")
                    .is_panic()
            );
        } else {
            assert_eq!(result.expect("native result remains intact"), 7);
        }
        drop(waiting);
        assert_eq!(super::system::FLASH.active_count(), 0);
        assert_eq!(forward::async_active_count(), 0);
        assert_eq!(super::system::FLASH.timed_count(), 0);
    }

    let StagedBlocking {
        mut handle,
        completion: _,
        run,
        exited,
        publish,
    } = staged(false);
    let mut borrowed = Box::pin(participate(&mut handle, Location::caller()));
    assert!(borrowed.as_mut().poll(&mut cx).is_pending());
    run.send(()).expect("finish borrowed join work");
    exited.recv().expect("borrowed pooled wrapper exited");
    publish.send(()).expect("publish borrowed native result");
    while forward::async_active_count() == 0 || super::system::FLASH.active_count() != 0 {
        thread::yield_now();
    }
    assert_eq!(forward::async_active_count(), 1);
    drop(borrowed);
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);

    let start = Instant::now();
    let mut waiting = Box::pin(participate(
        FlashTimeout {
            future: handle,
            sleep: FlashSleep::new(Duration::from_secs(2)),
        },
        Location::caller(),
    ));
    let mut waiting = rt
        .block_on(tokio::task::spawn(async move {
            let noop = Waker::from(Arc::new(NoopWake));
            let mut cx = Context::from_waker(&noop);
            loop {
                let mut budget = std::pin::pin!(tokio::task::consume_budget());
                if budget.as_mut().poll(&mut cx).is_pending() {
                    break;
                }
            }
            assert!(waiting.as_mut().poll(&mut cx).is_pending());
            assert_eq!(
                Instant::now().duration_since(start),
                Duration::ZERO,
                "a published join must hold credit across a deferred cooperative wake"
            );
            assert_eq!(forward::async_active_count(), 0);
            waiting
        }))
        .expect("poll published join with exhausted native budget");
    rt.block_on(tokio::task::yield_now());
    assert_eq!(
        Instant::now().duration_since(start),
        Duration::ZERO,
        "a deferred cooperative wake must hand credit to the runnable receiver"
    );
    assert_eq!(
        forward::async_active_count(),
        1,
        "a published join must forward its deferred cooperative wake"
    );
    let result = waiting.as_mut().poll(&mut cx);
    assert!(matches!(result, std::task::Poll::Ready(Ok(Ok(7)))));
    drop(waiting);
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);

    let (blocker_release, blocker_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        entered_tx.send(()).expect("occupy only pool thread");
        blocker_rx.recv().expect("release only pool thread");
    });
    entered.recv().expect("pool thread occupied");
    let queued =
        crate::tokio::task::spawn_blocking::<_, u8>(|| panic!("cancelled queued work ran"));
    let abort = queued.abort_handle();
    let mut waiting = Box::pin(participate(
        FlashTimeout {
            future: queued,
            sleep: FlashSleep::new(Duration::from_secs(2)),
        },
        Location::caller(),
    ));
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    abort.abort();
    blocker_release
        .send(())
        .expect("let pool settle cancelled work");
    while super::system::FLASH.active_count() != 0 {
        thread::yield_now();
    }
    let error = loop {
        if let std::task::Poll::Ready(result) = waiting.as_mut().poll(&mut cx) {
            break result
                .expect("cancelled join does not consume virtual budget")
                .expect_err("queued abort remains a join error");
        }
        thread::yield_now();
    };
    assert!(error.is_cancelled());
    drop(waiting);
    rt.block_on(blocker).expect("raw pool blocker joined");
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);

    for detached in [false, true] {
        let StagedBlocking {
            handle,
            completion: _,
            run,
            exited,
            publish,
        } = staged(false);
        let abort = handle.abort_handle();
        let retained = if detached {
            let mut waiting = Box::pin(participate(handle, Location::caller()));
            assert!(waiting.as_mut().poll(&mut cx).is_pending());
            drop(waiting);
            None
        } else {
            Some(handle)
        };
        run.send(()).expect("finish retained or detached work");
        exited.recv().expect("retained or detached pooled exit");
        assert_eq!(
            super::system::FLASH.active_count(),
            0,
            "an unawaited or detached result must not strand completion credit"
        );
        assert_eq!(forward::async_active_count(), 0);
        publish
            .send(())
            .expect("publish retained or detached result");
        while !abort.is_finished() {
            thread::yield_now();
        }
        if let Some(handle) = retained {
            assert_eq!(rt.block_on(handle).expect("retained result joins later"), 7);
        }
        assert_eq!(super::system::FLASH.active_count(), 0);
    }
}

#[kithara::test(native, flash(false))]
fn blocking_join_settles_credit_while_terminal_wake_is_in_flight() {
    struct PausedWake {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Wake for PausedWake {
        fn wake(self: Arc<Self>) {
            self.entered.send(()).expect("terminal wake entered");
            self.release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv()
                .expect("release terminal wake");
        }
    }

    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build controlled blocking runtime");
    let _rt = rt.enter();
    let (run, run_rx) = mpsc::channel();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let waker = Waker::from(Arc::new(PausedWake {
        entered,
        release: Mutex::new(release_rx),
    }));
    let mut cx = Context::from_waker(&waker);
    let mut handle = Box::pin(crate::tokio::task::spawn_blocking(move || {
        run_rx.recv().expect("release pooled work");
        7_u8
    }));
    assert!(handle.as_mut().poll(&mut cx).is_pending());
    run.send(()).expect("finish pooled work");
    entered_rx.recv().expect("terminal receiver wake is paused");

    let native_finished = handle.is_finished();
    let result = rt.block_on(handle);
    let active_after_join = super::system::FLASH.active_count();
    let async_active_after_join = forward::async_active_count();
    let timed_after_join = super::system::FLASH.timed_count();
    release.send(()).expect("resume terminal receiver wake");

    assert!(native_finished, "the native result was already published");
    assert_eq!(result.expect("published native result joins"), 7);
    assert_eq!(
        active_after_join, 0,
        "a completed join must settle its credit while the terminal wake is in flight"
    );
    assert_eq!(async_active_after_join, 0);
    assert_eq!(timed_after_join, 0);
}

#[kithara::test(native, flash(false))]
fn blocking_join_old_terminal_wake_preserves_new_pending_credit() {
    struct PausedFirstWake {
        calls: AtomicUsize,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Wake for PausedFirstWake {
        fn wake(self: Arc<Self>) {
            if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                self.entered.send(()).expect("first terminal wake entered");
                self.release
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv()
                    .expect("release first terminal wake");
            }
        }
    }

    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .expect("build single-worker blocking runtime");
    let _rt = rt.enter();
    let (run, run_rx) = mpsc::channel();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let notification = Arc::new(PausedFirstWake {
        calls: AtomicUsize::new(0),
        entered,
        release: Mutex::new(release_rx),
    });
    let waker = Waker::from(Arc::clone(&notification));
    let mut cx = Context::from_waker(&waker);
    let mut handle = Box::pin(crate::tokio::task::spawn_blocking(move || {
        run_rx.recv().expect("release pooled work");
        7_u8
    }));
    assert!(handle.as_mut().poll(&mut cx).is_pending());
    run.send(()).expect("finish pooled work");
    entered_rx
        .recv()
        .expect("first terminal receiver wake is paused");
    let native_finished = handle.is_finished();
    let (settled, settled_rx) = mpsc::channel();
    let sentinel = tokio::task::spawn_blocking(move || {
        settled.send(()).expect("old terminal callback completed");
    });
    let same_waker = waker.clone();
    let notification_in = Arc::clone(&notification);
    let (handle, pending, elapsed, active, calls) = rt
        .block_on(tokio::task::spawn(async move {
            let start = Instant::now_virtual();
            let noop = Waker::from(Arc::new(NoopWake));
            let mut noop_cx = Context::from_waker(&noop);
            let mut sibling = std::pin::pin!(FlashSleep::new(Duration::from_millis(10)));
            let sibling_pending = sibling.as_mut().poll(&mut noop_cx).is_pending();
            loop {
                let mut budget = std::pin::pin!(tokio::task::consume_budget());
                if budget.as_mut().poll(&mut noop_cx).is_pending() {
                    break;
                }
            }
            let mut same_cx = Context::from_waker(&same_waker);
            let pending = handle.as_mut().poll(&mut same_cx).is_pending();
            release.send(()).expect("resume old terminal receiver wake");
            settled_rx
                .recv()
                .expect("old terminal notification guard returned");
            let elapsed = Instant::now_virtual().duration_since(start);
            let active = super::system::FLASH.active_count();
            let calls = notification_in.calls.load(Ordering::Acquire);
            assert!(
                sibling_pending,
                "completion credit holds the sibling deadline"
            );
            (handle, pending, elapsed, active, calls)
        }))
        .expect("poll published join with exhausted native budget");
    rt.block_on(sentinel).expect("raw sentinel joined");
    assert!(native_finished, "the native result was already published");
    assert!(pending, "native cooperative budget deferred the new poll");
    assert_eq!(
        calls, 1,
        "the deferred wake cannot run within its current poll"
    );
    assert_eq!(
        elapsed,
        Duration::ZERO,
        "an old wake released the newer wait's clock credit"
    );
    assert_eq!(
        active, 1,
        "the newer pending join still owns completion credit"
    );
    rt.block_on(tokio::task::yield_now());
    assert!(notification.calls.load(Ordering::Acquire) >= 2);
    assert_eq!(
        rt.block_on(handle).expect("published native result joins"),
        7
    );
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);
}

#[kithara::test(native, flash(false))]
fn a_discarded_cooperative_join_delivery_releases_its_slot() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build controlled current-thread runtime");
    let _rt = rt.enter();
    let mut handle = Box::pin(crate::tokio::task::spawn_blocking(|| 7_u8));
    while !handle.is_finished() {
        thread::yield_now();
    }
    assert_eq!(super::system::FLASH.active_count(), 0);

    rt.block_on(async {
        let noop = Waker::from(Arc::new(NoopWake));
        let mut cx = Context::from_waker(&noop);
        while let std::task::Poll::Ready(spent) = tokio::task::coop::poll_proceed(&mut cx) {
            spent.made_progress();
        }
        assert!(handle.as_mut().poll(&mut cx).is_pending());
        assert_eq!(
            super::system::FLASH.active_count(),
            1,
            "a finished join retains the slot while cooperative delivery is queued"
        );
    });

    assert_eq!(
        super::system::FLASH.active_count(),
        0,
        "a root that returned without draining its deferred queue must release that delivery"
    );
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(
        rt.block_on(handle)
            .expect("the native result is still joinable"),
        7
    );
    assert_eq!(super::system::FLASH.active_count(), 0);
}

#[kithara::test(native, flash(false))]
fn a_pooled_backoff_releases_the_virtual_clock() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let _rt = rt.enter();
    let coordinator = super::system::FLASH.test_hold();
    let start = Instant::now_virtual();
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    let mut sibling = std::pin::pin!(FlashSleep::new(Duration::from_millis(10)));
    assert!(sibling.as_mut().poll(&mut cx).is_pending());
    let (run, run_rx) = mpsc::channel();
    let handle = crate::tokio::task::spawn_blocking(move || {
        run_rx.recv().expect("release pooled backoff");
        assert!(super::ambient_snapshot());
        assert!(super::ctx::dedicated());
        assert!(!flash_enabled());
        crate::thread::paced_backoff(Duration::ZERO);
        assert_eq!(
            Instant::now_virtual().duration_since(start),
            Duration::from_millis(10),
            "pooled backoff kept dedicated credit instead of releasing the sibling deadline"
        );
    });
    drop(coordinator);
    run.send(()).expect("start pooled backoff");
    rt.block_on(handle).expect("pooled backoff joined");
    assert!(sibling.as_mut().poll(&mut cx).is_ready());
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);
}

struct FinalDropPanicWake;

impl Wake for FinalDropPanicWake {
    fn wake(self: Arc<Self>) {}
}

impl Drop for FinalDropPanicWake {
    fn drop(&mut self) {
        panic!("controlled final receiver drop");
    }
}

struct CountWake(Arc<AtomicUsize>);

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[kithara::test(native, flash(false))]
fn blocking_join_drop_unwind_delivers_released_waiters() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build controlled blocking runtime");
    let _rt = rt.enter();
    let (run, run_rx) = mpsc::channel();
    let (exited_tx, exited) = mpsc::channel();
    let (publish, publish_rx) = mpsc::channel();
    let join = Join::new(true);
    let work = super::thread::joined_pool_task(
        move || {
            run_rx.recv().expect("release pooled work");
            7_u8
        },
        Some(Arc::clone(&join)),
    );
    let native = tokio::task::spawn_blocking(move || {
        let result = work();
        exited_tx.send(()).expect("report work exit");
        publish_rx.recv().expect("release native publication");
        result
    });
    let mut handle = Box::pin(JoinHandle::new(native, join));
    let native_finished = handle.abort_handle();
    let receiver = Waker::from(Arc::new(FinalDropPanicWake));
    assert!(
        handle
            .as_mut()
            .poll(&mut Context::from_waker(&receiver))
            .is_pending()
    );
    drop(receiver);
    run.send(()).expect("finish pooled work");
    exited.recv().expect("work exit handed over its slot");

    let calls = Arc::new(AtomicUsize::new(0));
    let waiter = Waker::from(Arc::new(CountWake(Arc::clone(&calls))));
    let mut sleep = std::pin::pin!(FlashSleep::new(Duration::from_millis(10)));
    assert!(
        sleep
            .as_mut()
            .poll(&mut Context::from_waker(&waiter))
            .is_pending()
    );
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(handle)));
    let delivered = calls.load(Ordering::Acquire);
    let active = super::system::FLASH.active_count();
    let ready = sleep
        .as_mut()
        .poll(&mut Context::from_waker(&waiter))
        .is_ready();
    let settled = super::system::FLASH.active_count();
    publish.send(()).expect("finish detached native work");
    while !native_finished.is_finished() {
        thread::yield_now();
    }

    assert!(cleanup.is_err(), "the final user receiver drop must unwind");
    assert_eq!(
        delivered, 1,
        "released engine waiters must be delivered before receiver drop"
    );
    assert_eq!(
        active, 1,
        "unwinding receiver destruction must settle completion credit, leaving only the released waiter's grant"
    );
    assert!(ready, "the released waiter's grant must reach its future");
    assert_eq!(settled, 0, "the consumed grant returns the last credit");
    assert_eq!(super::system::FLASH.timed_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
}

#[kithara::test(native, flash(false))]
fn blocking_join_replacement_drop_unwind_settles_new_registration() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build controlled current-thread runtime");
    let _rt = rt.enter();
    let mut handle = Box::pin(crate::tokio::task::spawn_blocking(|| 7_u8));
    while !handle.is_finished() {
        thread::yield_now();
    }
    assert_eq!(super::system::FLASH.active_count(), 0);

    rt.block_on(async {
        let noop = Waker::from(Arc::new(NoopWake));
        let mut cx = Context::from_waker(&noop);
        while let std::task::Poll::Ready(spent) = tokio::task::coop::poll_proceed(&mut cx) {
            spent.made_progress();
        }
        let receiver = Waker::from(Arc::new(FinalDropPanicWake));
        assert!(
            handle
                .as_mut()
                .poll(&mut Context::from_waker(&receiver))
                .is_pending()
        );
        drop(receiver);
        assert_eq!(super::system::FLASH.active_count(), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let waiter = Waker::from(Arc::new(CountWake(Arc::clone(&calls))));
        let mut sleep = std::pin::pin!(FlashSleep::new(Duration::from_millis(10)));
        assert!(
            sleep
                .as_mut()
                .poll(&mut Context::from_waker(&waiter))
                .is_pending()
        );
        let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = handle.as_mut().poll(&mut cx);
        }));

        assert!(
            cleanup.is_err(),
            "the replaced user receiver drop must unwind"
        );
        assert_eq!(
            super::system::FLASH.active_count(),
            1,
            "a borrowed handle must not strand its newer registration after replacement unwinds; only the released waiter's grant remains"
        );
        assert_eq!(
            calls.load(Ordering::Acquire),
            1,
            "registration cleanup must deliver the released engine waiter"
        );
        assert!(
            sleep
                .as_mut()
                .poll(&mut Context::from_waker(&waiter))
                .is_ready()
        );
        assert_eq!(
            super::system::FLASH.active_count(),
            0,
            "the consumed grant returns the last credit"
        );
    });
    assert_eq!(
        rt.block_on(handle)
            .expect("the retained native result remains joinable"),
        7
    );
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
}

#[kithara::test(native, flash(false))]
fn a_parked_async_abort_preserves_native_cancellation_without_credit() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build controlled async runtime");
    rt.block_on(async {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let handle = crate::tokio::task::spawn(async move {
            entered_tx.send(()).expect("report the task's first poll");
            std::future::pending::<()>().await;
        });
        entered.await.expect("the task was polled and parked");
        assert_eq!(forward::async_active_count(), 0);
        handle.abort();
        let error = handle
            .await
            .expect_err("parked abort remains a native join error");
        assert!(error.is_cancelled());
    });
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
    assert_eq!(super::system::FLASH.timed_count(), 0);
}

#[cfg(not(feature = "loom"))]
#[kithara::test(native, flash(false))]
fn synchronous_work_keeps_worker_deadlines_behind_its_completion() {
    let _g = guard();
    reset();
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let completed = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let wall_started = RealInstant::now();

    let waiter = super::participate_sync(|| {
        let worker_completed = Arc::clone(&completed);
        let waiter = crate::thread::spawn(move || {
            crate::thread::sleep(Duration::from_secs(1));
            worker_completed.store(1, Ordering::Release);
        });
        while super::system::FLASH.timed_count() == 0 && completed.load(Ordering::Acquire) == 0 {
            assert_fast(wall_started);
            thread::yield_now();
        }
        assert_eq!(completed.load(Ordering::Acquire), 0);
        assert_eq!(Instant::now(), started);
        super::participate_sync(|| assert_eq!(Instant::now(), started));
        crate::thread::sleep(Duration::from_millis(500));
        assert_eq!(Instant::now(), started + Duration::from_millis(500));
        assert_eq!(completed.load(Ordering::Acquire), 0);
        assert!(super::ctx::dedicated());
        waiter
    });

    waiter
        .join()
        .expect("timed worker completes after synchronous work");
    assert_eq!(completed.load(Ordering::Acquire), 1);
    assert!(Instant::now() >= started + Duration::from_secs(1));
    assert!(!super::ctx::dedicated());
    assert!(matches!(super::ctx::credit(), super::ctx::Credit::None));
    assert_fast(wall_started);
}

#[cfg(not(feature = "loom"))]
#[kithara::test(native, flash(false))]
fn synchronous_participation_reuses_async_credit_and_settles_unwinds() {
    let _g = guard();
    reset();
    super::participate_sync(|| assert_eq!(super::system::FLASH.active_count(), 0));
    let _a = ambient_scope(true);
    let _f = enter_dynamic(true);
    let ((), elapsed) = on_virtual_clock(async {
        super::participate_sync(|| {
            assert_eq!(super::system::FLASH.active_count(), 0);
            assert_eq!(forward::async_active_count(), 1);
            assert!(!super::ctx::dedicated());
        });
        crate::time::sleep(Duration::from_millis(10)).await;
    });
    assert_eq!(elapsed, Duration::from_millis(10));
    kithara_test_utils::hang::suppress_expected_panic_dumps();
    let result = std::panic::catch_unwind(|| {
        super::participate_sync(|| panic!("synchronous work failed"));
    });
    assert!(result.is_err());
    assert!(!super::ctx::dedicated());
    assert!(matches!(super::ctx::credit(), super::ctx::Credit::None));
    assert_eq!(super::system::FLASH.active_count(), 0);
    assert_eq!(forward::async_active_count(), 0);
}
