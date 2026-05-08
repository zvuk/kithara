use std::time::Duration;

use kithara_hang_detector::HangDump;
use kithara_test_utils::kithara;

#[kithara::test]
fn no_context_serializes_to_null() {
    assert_eq!(kithara_hang_detector::NoContext.to_json(), "null");
}

#[cfg(not(feature = "disable-hang-detector"))]
mod detector_tests {
    use std::{thread::sleep, time::Duration};

    use kithara_hang_detector::HangDetector;
    use kithara_test_utils::kithara;

    #[kithara::test]
    fn tick_within_timeout_does_not_panic() {
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            detector.tick();
        }
    }

    #[kithara::test(native)]
    #[should_panic(expected = "HangDetector")]
    fn tick_after_timeout_panics() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[kithara::test(wasm)]
    fn tick_after_timeout_does_not_panic_on_wasm() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[kithara::test]
    fn reset_extends_deadline() {
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_millis(50));
        sleep(Duration::from_millis(30));
        detector.reset();
        sleep(Duration::from_millis(30));
        detector.tick();
    }
}

#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
mod native_detector_tests {
    use std::{
        env,
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
        thread::sleep,
        time::Duration,
    };

    use kithara_hang_detector::HangDetector;
    use kithara_test_utils::kithara;

    use crate::detector::{fallback_timeout, parse_timeout_secs};

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

    #[kithara::test]
    fn fallback_timeout_is_ten_seconds() {
        assert_eq!(fallback_timeout(), Duration::from_secs(10));
    }

    #[kithara::test(native)]
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
            detector.tick_with(Ctx { phase: 5 });
            sleep(Duration::from_millis(10));
            detector.tick_with(Ctx { phase: 7 });
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
    .to_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["value"], 7);
    assert_eq!(parsed["name"], "x");
}

#[cfg(not(target_arch = "wasm32"))]
mod dump_tests {
    use std::path::PathBuf;

    use kithara_test_utils::kithara;

    use crate::dump::{resolve_dump_dir, sanitize_label, write_dump};

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

        let dir: PathBuf = std::env::temp_dir().join("kithara-hang-detector-dump-test");
        std::fs::create_dir_all(&dir).unwrap();

        write_dump(
            "tests::round::trip",
            &Ctx {
                kind: "sample",
                value: 42,
            },
            Some(dir.as_path()),
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

    #[kithara_hang_detector::hang_watchdog]
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

    #[kithara_hang_detector::hang_watchdog]
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

    #[kithara_hang_detector::hang_watchdog(name = "test.thread")]
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

    #[kithara_hang_detector::hang_watchdog(timeout = Duration::from_secs(1))]
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

    #[kithara_hang_detector::hang_watchdog(
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
