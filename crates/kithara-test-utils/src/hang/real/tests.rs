use kithara_platform::time::Duration;

use super::HangDump;
use crate::kithara;

#[kithara::test]
fn no_context_serializes_to_null() {
    assert_eq!(super::NoContext.dump_json(), "null");
}

#[kithara::test]
fn timeout_evidence_api_shape_is_stable() {
    let _write: fn(&str, &str) = super::record_test_hang;
    let _guard: fn(&str) -> super::PreKillGuard = super::PreKillGuard::new;
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

    /// Real-clock contract tests of the detector itself: the wait and the
    /// detector's internal `Instant` must read the SAME (real) clock, so the
    /// bodies stay un-rewritten via `flash(false)`.
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

    /// Real-clock contract test (see `detector_tests` above).
    #[kithara::test(native, flash(false))]
    fn tick_with_stores_context_for_dump() {
        #[derive(serde::Serialize)]
        struct Ctx {
            phase: u32,
        }

        let dir: PathBuf = env::temp_dir().join(format!(
            "kithara-hang-tick-with-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(parsed["schema"], "kithara.hang.v1");
        assert_eq!(parsed["label"], "tests.tick_with");
        assert!(parsed["diagnostic"].as_str().is_some());
        assert!(parsed["timestamp_ms"].as_u64().is_some());
        assert_eq!(parsed["pid"], std::process::id());
        assert_eq!(parsed["context"]["phase"], 7, "last tick_with wins");
        assert!(parsed["nextest"].is_object());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point of the watchdog upgrade: a fired panic names the exact
    /// `hang_tick!` call site (its <file:line>), not the detector internals. Real
    /// clock so the 1ms timeout and the detector's `Instant` read the same clock.
    #[kithara::test(native, flash(false))]
    fn panic_reports_hang_tick_call_site_not_detector_internals() {
        /// `hang_tick!()` sits exactly one line below the `line!()` marker, so
        /// the captured expectation equals the line the macro forwards via
        /// `file!()`/`line!()` — exact and stable under rustfmt.
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

    use super::super::{HangDump, resolve_dump_dir, sanitize_label, write_dump};
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
    fn sanitize_label_bounds_filename_component() {
        let sanitized = sanitize_label(&"x".repeat(1_000));
        assert_eq!(sanitized.len(), 96);
        assert_eq!(sanitize_label(""), "unknown");
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
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(parsed["schema"], "kithara.hang.v1");
        assert_eq!(parsed["label"], "tests::round::trip");
        assert_eq!(parsed["diagnostic"], "stuck at x.rs:1");
        assert!(parsed.get("flash").is_some());
        if let Some(flash) = parsed["flash"].as_str() {
            assert!(flash.contains("[flash hang dump]"));
            assert!(flash.contains("tests::round::trip"));
        }
        assert!(parsed["timestamp_ms"].as_u64().is_some());
        assert_eq!(parsed["pid"], std::process::id());
        assert_eq!(parsed["context"]["kind"], "sample");
        assert_eq!(parsed["context"]["value"], 42);
        assert!(parsed["nextest"].is_object());
        for (field, env_name) in [
            ("run_id", "NEXTEST_RUN_ID"),
            ("attempt_id", "NEXTEST_ATTEMPT_ID"),
            ("test_name", "NEXTEST_TEST_NAME"),
            ("stress_current", "NEXTEST_STRESS_CURRENT"),
            ("stress_total", "NEXTEST_STRESS_TOTAL"),
        ] {
            let expected = std::env::var(env_name)
                .ok()
                .filter(|value| !value.is_empty());
            assert_eq!(
                parsed["nextest"][field].as_str(),
                expected.as_deref(),
                "nextest field {field} must match {env_name}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[kithara::test]
    fn write_dump_uses_collision_resistant_names() {
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "kithara-hang-detector-collision-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_dump("same", &serde_json::json!({"call": 1}), Some(&dir), "one");
        write_dump("same", &serde_json::json!({"call": 2}), Some(&dir), "two");

        let mut names = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[kithara::test]
    fn write_dump_preserves_non_json_context_as_a_string() {
        struct InvalidJson;

        impl HangDump for InvalidJson {
            fn dump_json(&self) -> String {
                "not-json".to_owned()
            }
        }

        let dir: PathBuf = std::env::temp_dir().join(format!(
            "kithara-hang-detector-invalid-json-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_dump("invalid", &InvalidJson, Some(&dir), "diagnostic");

        let dump = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .next()
            .expect("no dump file produced");
        let body = std::fs::read_to_string(dump.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["context"], "not-json");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod panic_dump_tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
    };

    use kithara_platform::{thread::sleep, time::Duration};
    use tracing_subscriber::layer::SubscriberExt;

    use super::super::{HangDetector, install_panic_dump, suppress_expected_panic_dumps};
    use crate::kithara;

    /// Panic dumps land where `resolve_dump_dir` sends them with no explicit
    /// dir and no env override: the system temp directory. Filenames carry the
    /// pid, and each test filters by its own unique panic message.
    fn panic_dumps_containing(needle: &str) -> Vec<PathBuf> {
        let pid = format!("-{}-", std::process::id());
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("kithara-hang-panic-") && name.contains(&pid)
            })
            .map(|entry| entry.path())
            .filter(|path| std::fs::read_to_string(path).is_ok_and(|body| body.contains(needle)))
            .collect()
    }

    #[kithara::test]
    fn assertion_panic_records_a_dump_with_the_flight_tail() {
        install_panic_dump();
        let marker = format!("flight-marker-{}", std::process::id());
        let probe_marker = format!("probe-marker-{}", std::process::id());
        let subscriber = tracing_subscriber::registry().with(crate::flight::layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "kithara_panic_dump_test", "{marker}");
            tracing::trace!(
                target: "kithara_panic_dump_test_probe",
                probe = probe_marker.as_str(),
                "probe firing"
            );
        });

        let unique = format!("panic-dump-probe-{}", std::process::id());
        let result = catch_unwind(AssertUnwindSafe(|| panic!("{unique} boom")));
        assert!(result.is_err());

        let dumps = panic_dumps_containing(&unique);
        assert!(!dumps.is_empty(), "unexpected panic must write a dump");
        let body = std::fs::read_to_string(&dumps[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["label"], "panic");
        let diagnostic = parsed["diagnostic"].as_str().unwrap();
        assert!(diagnostic.contains("tests.rs:"), "{diagnostic}");
        let events = parsed["flight_events"].as_array().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.as_str().is_some_and(|line| line.contains(&marker))),
            "dump must carry the flight-recorder tail"
        );
        let probes = parsed["flight_probes"].as_array().unwrap();
        assert!(
            probes.iter().any(|event| {
                event
                    .as_str()
                    .is_some_and(|line| line.contains(&probe_marker))
            }),
            "dump must carry the probe tail"
        );
        for dump in dumps {
            let _ = std::fs::remove_file(dump);
        }
    }

    /// Real clock: the detector's deadline and the sleep must agree (see the
    /// detector contract tests above).
    #[kithara::test(native, flash(false))]
    fn watchdog_panic_is_not_duplicated_as_a_panic_dump() {
        install_panic_dump();
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "kithara-hang-panic-suppress-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_for_closure = dir.clone();

        let marker = format!("watchdog-flight-marker-{}", std::process::id());
        let subscriber = tracing_subscriber::registry().with(crate::flight::layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "kithara_panic_dump_test", "{marker}");
        });

        let result = catch_unwind(AssertUnwindSafe(move || {
            let mut detector: HangDetector =
                HangDetector::new("tests.panic_suppress", Duration::from_millis(1))
                    .with_dump_dir(dir_for_closure);
            detector.tick();
            sleep(Duration::from_millis(10));
            detector.tick();
        }));
        assert!(result.is_err(), "detector must panic past deadline");

        let own_dumps: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            own_dumps.len(),
            1,
            "the watchdog writes exactly its own dump"
        );
        assert!(
            panic_dumps_containing("tests.panic_suppress").is_empty(),
            "the watchdog panic must not produce a second `panic` dump"
        );
        let body = std::fs::read_to_string(&own_dumps[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            parsed["flight_events"].as_array().is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event.as_str().is_some_and(|line| line.contains(&marker)))
            }),
            "the watchdog dump must carry the flight-recorder tail"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[kithara::test]
    fn expected_panics_are_not_recorded() {
        install_panic_dump();
        suppress_expected_panic_dumps();

        let unique = format!("expected-panic-probe-{}", std::process::id());
        let result = catch_unwind(AssertUnwindSafe(|| panic!("{unique} expected")));
        assert!(result.is_err());

        assert!(
            panic_dumps_containing(&unique).is_empty(),
            "an expected panic must not be recorded as evidence"
        );
    }

    /// A payload that is neither `&str` nor `String` is a dependency unwinding
    /// for control flow rather than a failing test: `loom` cancels every
    /// suspended generator that way at the end of each execution.
    #[kithara::test]
    fn control_flow_unwinds_are_not_recorded() {
        install_panic_dump();

        let line = line!() + 1;
        let result = catch_unwind(AssertUnwindSafe(|| std::panic::panic_any(0xdead_beef_u64)));
        assert!(result.is_err());

        assert!(
            panic_dumps_containing(&format!("tests.rs:{line}")).is_empty(),
            "a control-flow unwind must not be recorded as evidence"
        );
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
