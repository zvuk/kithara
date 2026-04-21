use std::time::Duration;

use kithara_hang_detector::HangDump;
use kithara_test_utils::kithara;

#[kithara::test]
fn no_context_serializes_to_null() {
    assert_eq!(kithara_hang_detector::NoContext.to_json(), "null");
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
    const LOOP_BREAK_COUNT_3: i32 = 3;
    const LOOP_BREAK_COUNT_2: i32 = 2;
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
