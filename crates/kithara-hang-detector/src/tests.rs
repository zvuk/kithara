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
