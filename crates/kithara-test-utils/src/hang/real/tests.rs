use kithara_platform::time::Duration;

use super::HangDump;
use crate::kithara;

#[kithara::test]
fn no_context_serializes_to_null() {
    assert_eq!(super::NoContext.dump_json(), "null");
}

mod detector_tests {
    use kithara_platform::{thread::sleep, time::Duration};

    use super::super::HangDetector;
    use crate::kithara;

    #[kithara::test]
    fn tick_within_timeout_does_not_panic() {
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            detector.tick();
        }
    }

    // Real-clock contract tests of the detector itself: the wait and the
    // detector's internal `Instant` must read the SAME (real) clock, so the
    // bodies stay un-rewritten via `flash(false)`.
    #[kithara::test(native, flash(false))]
    #[should_panic(expected = "HangDetector")]
    fn tick_after_timeout_panics() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        // The liveness budget starts at the FIRST observation (lazy deadline
        // stamp), not at construction: time spent before the watched loop is
        // entered must not count against it.
        detector.tick();
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[kithara::test(wasm, flash(false))]
    fn tick_after_timeout_does_not_panic_on_wasm() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[kithara::test(flash(false))]
    fn reset_extends_deadline() {
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_millis(50));
        sleep(Duration::from_millis(30));
        detector.reset();
        sleep(Duration::from_millis(30));
        detector.tick();
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_detector_tests {
    use std::{
        env,
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
    };

    use kithara_platform::{thread::sleep, time::Duration};

    use super::super::{HangDetector, parse_timeout_secs};
    use crate::kithara;

    #[kithara::test]
    fn parse_timeout_secs_rejects_invalid() {
        assert_eq!(parse_timeout_secs(""), None);
        assert_eq!(parse_timeout_secs("abc"), None);
        assert_eq!(parse_timeout_secs("0"), None);
    }

    #[kithara::test]
    fn parse_timeout_secs_accepts_positive_numbers() {
        assert_eq!(parse_timeout_secs("7"), Some(Duration::from_secs(7)));
    }

    // Real-clock contract test (see detector_tests above).
    #[kithara::test(native, flash(false))]
    fn tick_with_stores_context_for_dump() {
        #[derive(serde::Serialize)]
        struct Ctx {
            phase: u32,
        }

        let dir: PathBuf = env::temp_dir().join("kithara-hang-tick-with-test");
        let _ = std::fs::create_dir_all(&dir);
        let dir_for_closure = dir.clone();

        let result = catch_unwind(AssertUnwindSafe(move || {
            let mut detector: HangDetector<Ctx> =
                HangDetector::new("tests.tick_with", Duration::from_millis(1))
                    .with_dump_dir(dir_for_closure);
            detector.tick_with(|| Ctx { phase: 5 });
            sleep(Duration::from_millis(10));
            detector.tick_with(|| Ctx { phase: 7 });
        }));
        assert!(result.is_err(), "detector must panic past deadline");

        let newest = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("kithara-hang-tests.tick_with-")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).unwrap())
            .expect("no dump file produced");
        let body = std::fs::read_to_string(newest.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["phase"], 7, "last tick_with wins");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The whole point of the watchdog upgrade: a fired panic names the exact
    // `hang_tick!` call site (its file:line), not the detector internals. Real
    // clock so the 1ms timeout and the detector's `Instant` read the same clock.
    #[kithara::test(native, flash(false))]
    fn panic_reports_hang_tick_call_site_not_detector_internals() {
        // `hang_tick!()` sits exactly one line below the `line!()` marker, so
        // the captured expectation equals the line the macro forwards via
        // `file!()`/`line!()` — exact and stable under rustfmt.
        #[kithara::hang_watchdog(timeout = Duration::from_millis(1))]
        fn spin(tick_line: &mut u32) {
            loop {
                sleep(Duration::from_millis(5));
                *tick_line = line!() + 1;
                hang_tick!();
            }
        }

        let mut tick_line = 0u32;
        let payload = catch_unwind(AssertUnwindSafe(|| spin(&mut tick_line)))
            .expect_err("spin must panic past deadline");
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .expect("panic payload is a string")
            .to_string();

        assert!(msg.contains("HangDetector"), "msg: {msg}");
        assert!(
            msg.contains(&format!(
                "kithara-test-utils/src/hang/real/tests.rs:{tick_line}"
            )) || msg.contains(&format!("tests.rs:{tick_line}")),
            "panic must name the exact hang_tick! line {tick_line}: {msg}"
        );
        assert!(
            !msg.contains("detector_native.rs"),
            "panic must not report detector internals: {msg}"
        );
        // No `hang_reset!` ran, so last progress is unknown but still reported.
        assert!(
            msg.contains("last progress at <unknown>"),
            "diagnostic must report the last-progress location: {msg}"
        );
    }
}

#[kithara::test]
fn blanket_impl_serializes_serde_type() {
    #[derive(serde::Serialize)]
    struct Ctx {
        value: i32,
        name: &'static str,
    }
    let json = Ctx {
        value: 7,
        name: "x",
    }
    .dump_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["value"], 7);
    assert_eq!(parsed["name"], "x");
}

#[cfg(not(target_arch = "wasm32"))]
mod dump_tests {
    use std::path::PathBuf;

    use super::super::{resolve_dump_dir, sanitize_label, write_dump};
    use crate::kithara;

    #[kithara::test]
    fn sanitize_label_preserves_safe_chars() {
        assert_eq!(sanitize_label("abc_123-XYZ"), "abc_123-XYZ");
    }

    #[kithara::test]
    fn sanitize_label_replaces_path_separators() {
        assert_eq!(
            sanitize_label("kithara_audio::audio::read"),
            "kithara_audio..audio..read"
        );
        assert_eq!(sanitize_label("/etc/passwd"), ".etc.passwd");
    }

    #[kithara::test]
    fn resolve_dump_dir_precedence_explicit_wins() {
        let explicit = PathBuf::from("/tmp/kithara-explicit");
        let resolved = resolve_dump_dir(Some(&explicit));
        assert_eq!(resolved, explicit);
    }

    #[kithara::test]
    fn write_dump_produces_readable_json() {
        #[derive(serde::Serialize)]
        struct Ctx {
            kind: &'static str,
            value: i64,
        }

        // Per-process subdir (nextest is process-per-test) so concurrent tests
        // and stale runs never share this scratch path.
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "kithara-hang-detector-dump-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        write_dump(
            "tests::round::trip",
            &Ctx {
                kind: "sample",
                value: 42,
            },
            Some(dir.as_path()),
            "stuck at x.rs:1",
        );

        let newest = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("kithara-hang-tests..round..trip-")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).unwrap())
            .expect("no dump file produced");

        let body = std::fs::read_to_string(newest.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["kind"], "sample");
        assert_eq!(parsed["value"], 42);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

struct Consts;
impl Consts {
    const LOOP_BREAK_COUNT_2: i32 = 2;
    const LOOP_BREAK_COUNT_3: i32 = 3;
}

#[kithara::test]
fn attr_macro_loop_compiles_and_runs() {
    let mut count = 0;

    #[kithara::hang_watchdog]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= Consts::LOOP_BREAK_COUNT_3 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, Consts::LOOP_BREAK_COUNT_3);
}

#[kithara::test]
fn attr_macro_while_compiles_and_runs() {
    let mut count = 0;

    #[kithara::hang_watchdog]
    fn run_while(count: &mut i32) {
        while *count < Consts::LOOP_BREAK_COUNT_3 {
            *count += 1;
            hang_reset!();
            hang_tick!();
        }
    }

    run_while(&mut count);
    assert_eq!(count, Consts::LOOP_BREAK_COUNT_3);
}

#[kithara::test]
fn attr_macro_with_thread_compiles_and_runs() {
    let mut count = 0;

    #[kithara::hang_watchdog(name = "test.thread")]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= Consts::LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, Consts::LOOP_BREAK_COUNT_2);
}

#[kithara::test]
fn attr_macro_with_timeout_compiles_and_runs() {
    let mut count = 0;

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= Consts::LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, Consts::LOOP_BREAK_COUNT_2);
}

#[kithara::test]
fn attr_macro_with_thread_and_timeout_compiles_and_runs() {
    let mut count = 0;

    #[kithara::hang_watchdog(
        name = "test.thread",
        timeout = Duration::from_secs(1)
    )]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= Consts::LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, Consts::LOOP_BREAK_COUNT_2);
}
