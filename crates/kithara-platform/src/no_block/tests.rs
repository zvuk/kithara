use std::{
    env, fs,
    future::Future,
    path::PathBuf,
    pin::pin,
    process,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use super::{
    mode::{Mode, force_log_path, force_mode},
    *,
};

const FIRST_LOG_FILE_ID: usize = 0;
const CENSUS_LOG_BUDGET_MS: u64 = 10_000;
const CENSUS_LOG_SLEEP_MS: u64 = 1;

static LOG_FILE_ID: AtomicUsize = AtomicUsize::new(FIRST_LOG_FILE_ID);

fn poll_once<F: Future>(fut: F) -> Poll<F::Output> {
    let mut fut = pin!(fut);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    fut.as_mut().poll(&mut cx)
}

fn temp_log_path(name: &str) -> PathBuf {
    let mut path = env::temp_dir();
    let id = LOG_FILE_ID.fetch_add(1, Ordering::Relaxed);
    path.push(format!(
        "kithara-no-block-{name}-{}-{id}.log",
        process::id()
    ));
    path
}

fn spin_for(d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}

#[test]
fn budget_flags_over_budget_poll() {
    force_mode(Mode::Panic);

    let caught = std::panic::catch_unwind(|| {
        let fut = watch_budget("spin_task", 10, async {
            spin_for(Duration::from_millis(50));
        });
        let _ = poll_once(fut);
    });
    let err = caught.expect_err("over-budget spin poll must panic");
    let msg = err.downcast_ref::<String>().expect("panic payload");
    assert!(msg.contains("[no_block]"), "got: {msg}");
    assert!(msg.contains("spin_task"), "got: {msg}");
    assert!(msg.contains("budget"), "got: {msg}");
}

#[test]
fn budget_ignores_paused_time() {
    force_mode(Mode::Panic);

    let fut = watch_budget("paused_task", 10, async {
        let _p = permit();
        spin_for(Duration::from_millis(50));
    });
    let _ = poll_once(fut);
}

#[test]
fn fast_poll_passes() {
    force_mode(Mode::Panic);

    assert!(matches!(
        poll_once(watch_budget("ok", 10, async {})),
        Poll::Ready(())
    ));
}

#[test]
fn census_writes_to_forced_log_path() {
    force_mode(Mode::Census);

    let path = temp_log_path("census");
    let _ = fs::remove_file(&path);
    force_log_path(path.clone());

    let fut = watch_budget("census_file_task", CENSUS_LOG_BUDGET_MS, async {
        crate::thread::sleep(Duration::from_millis(CENSUS_LOG_SLEEP_MS));
    });
    let _ = poll_once(fut);

    let contents = fs::read_to_string(&path).expect("read census log");
    assert!(contents.contains("census_file_task"), "got: {contents}");
    let _ = fs::remove_file(path);
}

#[test]
fn forbid_fires_on_platform_sleep_inside_poll() {
    force_mode(Mode::Panic);

    let caught = std::panic::catch_unwind(|| {
        let fut = watch_budget("sleeper", 10_000, async {
            crate::thread::sleep(Duration::from_millis(1));
        });
        let _ = poll_once(fut);
    });
    let err = caught.expect_err("platform sleep inside poll must hit forbid");
    let msg = err.downcast_ref::<String>().expect("panic payload");
    assert!(msg.contains("thread::sleep"), "got: {msg}");
    assert!(msg.contains("sleeper"), "got: {msg}");
    assert!(
        msg.contains("tests.rs"),
        "forbid must attribute the call site, got: {msg}"
    );
}

#[test]
fn allow_block_permit_suppresses_forbid() {
    force_mode(Mode::Panic);

    let fut = watch_budget("permitted_sleeper", 10_000, async {
        let _permit = permit();
        crate::thread::sleep(Duration::from_millis(1));
    });
    let _ = poll_once(fut);
}

#[test]
fn permit_poll_suppresses_forbid_and_budget() {
    force_mode(Mode::Panic);

    let fut = watch_budget(
        "outer",
        10,
        permit_poll(async {
            crate::thread::sleep(Duration::from_millis(30));
        }),
    );
    let _ = poll_once(fut);
}

#[test]
fn forbid_still_fires_after_permit_poll_scope_ends() {
    force_mode(Mode::Panic);

    let permitted = watch_budget(
        "permitted",
        10,
        permit_poll(async {
            crate::thread::sleep(Duration::from_millis(1));
        }),
    );
    let _ = poll_once(permitted);

    let caught = std::panic::catch_unwind(|| {
        let fut = watch_budget("plain", 10_000, async {
            crate::thread::sleep(Duration::from_millis(1));
        });
        let _ = poll_once(fut);
    });
    let err = caught.expect_err("plain sleep after permit_poll must hit forbid");
    let msg = err.downcast_ref::<String>().expect("panic payload");
    assert!(msg.contains("thread::sleep"), "got: {msg}");
    assert!(msg.contains("plain"), "got: {msg}");
}

#[test]
fn sleep_outside_poll_is_untouched() {
    force_mode(Mode::Panic);

    crate::thread::sleep(Duration::from_millis(1));
}
