#![forbid(unsafe_code)]

use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara::test as kithara_test;
use kithara_platform::time::{Duration, Instant};

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
            && entry.file_type().is_ok_and(|t| t.is_file())
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

// flash(false): measures a real subprocess's wall-clock EOF->exit gap (nextest
// LEAK heuristic). Its `Instant::now`/`sleep`/spin are wall-clock by nature; the
// virtual clock cannot model a separate OS process, so this must run real.
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

const CONTENDERS: usize = 32;

/// 32 real OS threads that busy-spin on the REAL wall clock, plus the latch we
/// use to confirm they have all entered their spin loop.
///
/// This lives in a helper (not the test body) on purpose: the `flash(true)`
/// body rewriter lexically retargets `Instant::now()` in the TEST BODY onto the
/// engine's `virtual_now()`. These workers are raw OS threads that never park on
/// the virtual clock, so a virtual `Instant::now()` would freeze (the engine
/// only advances when all flash tasks park) and the inner spin loop would never
/// terminate. Keeping the spin in a helper leaves `Instant::now()` on the REAL
/// clock — exactly as `spawn_and_measure_leak_gap` does for its own wall-clock
/// reads — so the contention is real, matching what the nextest LEAK heuristic
/// (also wall-clock) measures.
struct ContentionWorkers {
    stop: Arc<AtomicU64>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl ContentionWorkers {
    /// Spawn the contenders and return once all of them have actually entered
    /// their spin loop. The wait is on real coordination STATE (`started`
    /// reaching `CONTENDERS`), the same observable the threads produce; the
    /// `Instant` deadline is only a hang guard, not a pacing wait. Both reads
    /// are REAL here because this is a helper, not the rewritten test body.
    fn spawn_and_ramp_up() -> Self {
        let stop = Arc::new(AtomicU64::new(0));
        let started = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::with_capacity(CONTENDERS);
        for _ in 0..CONTENDERS {
            let stop = Arc::clone(&stop);
            let started = Arc::clone(&started);
            handles.push(std::thread::spawn(move || {
                started.fetch_add(1, Ordering::Relaxed);
                while stop.load(Ordering::Relaxed) == 0 {
                    let deadline = Instant::now() + Duration::from_micros(200);
                    while Instant::now() < deadline {
                        std::hint::spin_loop();
                    }
                    std::thread::yield_now();
                }
            }));
        }

        let ramp_guard = Instant::now() + Duration::from_secs(5);
        while started.load(Ordering::Relaxed) < CONTENDERS as u64 {
            assert!(
                Instant::now() < ramp_guard,
                "contender threads failed to start within 5s",
            );
            std::thread::yield_now();
        }

        Self { stop, handles }
    }

    /// Signal the contenders to stop and join them.
    fn stop_and_join(self) {
        self.stop.store(1, Ordering::Relaxed);
        for h in self.handles {
            h.join().ok();
        }
    }
}

// flash(true) like the rest of the suite: every wall-clock read lives in a
// helper (`ContentionWorkers`, `spawn_and_measure_leak_gap`), so the body's
// flash rewrite has no `Instant::now()` to retarget onto the (never-advancing)
// virtual clock. The 32 real spin threads + the subprocess EOF->exit gap stay on
// the REAL clock, where the nextest LEAK heuristic actually measures them.
#[kithara_test(native)]
fn red_env_guard_no_leak_under_env_lock_contention() {
    let workers = ContentionWorkers::spawn_and_ramp_up();

    let gap = spawn_and_measure_leak_gap();

    workers.stop_and_join();

    assert!(
        gap <= Duration::from_millis(100),
        "post-EOF -> exit gap {gap:?} under CPU contention exceeds \
         nextest leak-timeout (100ms) — matches the LEAK[0.271s] signature",
    );
}
