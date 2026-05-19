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
    const CONTENDERS: usize = 32;

    let stop = Arc::new(AtomicU64::new(0));
    let mut workers = Vec::with_capacity(CONTENDERS);
    for _ in 0..CONTENDERS {
        let stop = Arc::clone(&stop);
        workers.push(std::thread::spawn(move || {
            while stop.load(Ordering::Relaxed) == 0 {
                let deadline = Instant::now() + Duration::from_micros(200);
                while Instant::now() < deadline {
                    std::hint::spin_loop();
                }
                std::thread::yield_now();
            }
        }));
    }

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
