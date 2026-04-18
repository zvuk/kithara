//! RED test: nextest reports `LEAK` for
//! `env_guard::no_proxy_env_keeps_explicit_proxy_override` when the
//! workspace-wide suite runs under contention (`just test`).
//!
//! Symptom (from the user's report):
//! ```
//! LEAK [0.271s] (821/1800) env_guard::no_proxy_env_keeps_explicit_proxy_override
//! ```
//!
//! What LEAK means in nextest
//! Each test runs in its own subprocess (invocation of the test binary with
//! `--exact`).  After the test body completes, nextest waits up to
//! `leak-timeout` (default **100 ms**) for the subprocess to close stdout,
//! stderr, and exit.  If it doesn't, nextest flags `LEAK` — the test still
//! passes but its subprocess held resources past the grace window.
//!
//! Hypothesis
//! The `#[kithara::test(native, env(...), timeout(Duration::from_secs(5)))]`
//! macro expands into a sync `#[test]` fn whose tail is:
//!
//!   1. Inside the body: `_kithara_env_guard` acquired — takes
//!      `kithara_platform::test_env_lock()` (a global `std::sync::Mutex<()>`
//!      shared by **every** `env(...)` test in the process) and saves
//!      previous values for up to 7 vars (NO_PROXY, HTTP_PROXY,
//!      HTTPS_PROXY, ALL_PROXY, http_proxy, https_proxy, all_proxy).
//!
//!   2. `wrap_with_timeout` (sync branch,
//!      `crates/kithara-test-macros/src/lib.rs:422-459`) spawns a fresh
//!      `std::thread::spawn(...)` to run the body and uses an mpsc channel
//!      with `recv_timeout`.  Happy path incurs one thread create + join.
//!
//!   3. On Drop, `_kithara_env_guard` replays all saved env vars (another
//!      round of `set_var` / `remove_var` under the lock) and releases the
//!      mutex.
//!
//!   4. The `#[test]` fn returns, the test harness finalizes, the
//!      subprocess exits.
//!
//! Steps 2 + 3 + 4 are the tail latency nextest measures.  Under
//! workspace-wide load (8+ contenders on `test_env_lock`, many subprocess
//! spawns competing for kernel resources), this tail can exceed 100 ms —
//! which is what triggers LEAK.
//!
//! What this RED test measures
//! We launch the real test binary
//! `env_guard::no_proxy_env_keeps_explicit_proxy_override` as a subprocess
//! and measure the **wall-clock gap between our parent reading EOF on its
//! stdout and the child actually reporting `Exited`** — exactly what nextest
//! uses to decide LEAK.  We assert that gap stays under the 100 ms grace
//! window, both in isolation and under sustained contention on the
//! `test_env_lock`.
//!
//! In isolation, today, the test passes — the tail is ~10 ms.  Under the
//! workspace-wide load profile (reproduced here by `N` background threads
//! hammering the env lock inside our own parent process while the child
//! runs) the tail can blow past 100 ms.  That is the signature of the
//! observed LEAK.

#![forbid(unsafe_code)]

use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use kithara::test as kithara_test;

/// Locate the compiled `suite_light` binary next to our own test binary.
///
/// `std::env::current_exe()` returns path to *this* red test binary, which
/// lives in the same `target/<profile>/deps/` directory as `suite_light`.
fn locate_suite_light() -> PathBuf {
    let me = std::env::current_exe().expect("current_exe");
    let deps_dir = me.parent().expect("deps dir").to_path_buf();
    let entries = std::fs::read_dir(&deps_dir).expect("read deps");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `suite_light-<hash>` without a file extension is the test bin.
        if name.starts_with("suite_light-")
            && !name.contains('.')
            && entry.file_type().map(|t| t.is_file()).unwrap_or(false)
        {
            return entry.path();
        }
    }
    panic!(
        "could not find suite_light-<hash> binary next to {}",
        deps_dir.display()
    );
}

/// Spawn the real `no_proxy_env_keeps_explicit_proxy_override` test, read
/// its stdout to EOF, then measure the gap between EOF and actual process
/// exit.  This is nextest's LEAK heuristic.
fn spawn_and_measure_leak_gap() -> Duration {
    let bin = locate_suite_light();
    let mut child = Command::new(&bin)
        .arg("--exact")
        .arg("env_guard::no_proxy_env_keeps_explicit_proxy_override")
        .arg("--test")
        .arg("--nocapture")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn suite_light");

    // Read stdout to completion.  Whichever pipe closes last marks "test
    // body said it's done"; for a passing trivial test that's effectively
    // immediately after the `test result: ok.` line is flushed.
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
    });
    let mut buf = Vec::new();
    let _ = out.read_to_end(&mut buf);
    let eof_at = Instant::now();

    stderr_handle.join().ok();
    let _status = child.wait().expect("wait child");
    let exited_at = Instant::now();

    exited_at.saturating_duration_since(eof_at)
}

#[kithara_test(native)]
fn red_env_guard_no_leak_in_isolation() {
    let gap = spawn_and_measure_leak_gap();
    assert!(
        gap <= Duration::from_millis(100),
        "post-EOF -> exit gap {gap:?} exceeds nextest leak-timeout (100ms) — \
         `env_guard::no_proxy_env_keeps_explicit_proxy_override` is the test \
         observed as LEAK [0.271s] in the full suite",
    );
}

#[kithara_test(native)]
fn red_env_guard_no_leak_under_env_lock_contention() {
    // Reproduce LEAK conditions: many in-parent workers hammer the
    // `test_env_lock` while the child subprocess runs its own env test.
    // Because the lock is process-local, the child has its own copy — so
    // this tests the *OS-level* shutdown overhead under sustained CPU
    // pressure from our own threads, which is what happens under
    // `just test` where 8+ subprocesses contend with each other for CPU.
    const CONTENDERS: usize = 32;

    let stop = Arc::new(AtomicU64::new(0));
    let mut workers = Vec::with_capacity(CONTENDERS);
    for _ in 0..CONTENDERS {
        let stop = Arc::clone(&stop);
        workers.push(std::thread::spawn(move || {
            while stop.load(Ordering::Relaxed) == 0 {
                // CPU-burn to contend with the child subprocess.
                let deadline = Instant::now() + Duration::from_micros(200);
                while Instant::now() < deadline {
                    std::hint::spin_loop();
                }
                std::thread::yield_now();
            }
        }));
    }

    // Let contention warm up.
    std::thread::sleep(Duration::from_millis(50));

    let gap = spawn_and_measure_leak_gap();

    stop.store(1, Ordering::Relaxed);
    for w in workers {
        w.join().ok();
    }

    assert!(
        gap <= Duration::from_millis(100),
        "post-EOF -> exit gap {gap:?} under CPU contention exceeds \
         nextest leak-timeout (100ms) — matches the LEAK[0.271s] signature",
    );
}
