use std::time::Duration;

const LOOP_BREAK_COUNT_3: i32 = 3;
const LOOP_BREAK_COUNT_2: i32 = 2;

#[test]
fn attr_macro_loop_compiles_and_runs() {
    let mut count = 0;

    #[kithara_hang_detector::hang_watchdog]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= LOOP_BREAK_COUNT_3 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, LOOP_BREAK_COUNT_3);
}

#[test]
fn attr_macro_while_compiles_and_runs() {
    let mut count = 0;

    #[kithara_hang_detector::hang_watchdog]
    fn run_while(count: &mut i32) {
        while *count < LOOP_BREAK_COUNT_3 {
            *count += 1;
            hang_reset!();
            hang_tick!();
        }
    }

    run_while(&mut count);
    assert_eq!(count, LOOP_BREAK_COUNT_3);
}

#[test]
fn attr_macro_with_thread_compiles_and_runs() {
    let mut count = 0;

    #[kithara_hang_detector::hang_watchdog(name = "test.thread")]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, LOOP_BREAK_COUNT_2);
}

#[test]
fn attr_macro_with_timeout_compiles_and_runs() {
    let mut count = 0;

    #[kithara_hang_detector::hang_watchdog(timeout = Duration::from_secs(1))]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, LOOP_BREAK_COUNT_2);
}

#[test]
fn attr_macro_with_thread_and_timeout_compiles_and_runs() {
    let mut count = 0;

    #[kithara_hang_detector::hang_watchdog(
        name = "test.thread",
        timeout = Duration::from_secs(1)
    )]
    fn run_loop(count: &mut i32) {
        loop {
            *count += 1;
            if *count >= LOOP_BREAK_COUNT_2 {
                break;
            }
            hang_reset!();
            hang_tick!();
        }
    }

    run_loop(&mut count);
    assert_eq!(count, LOOP_BREAK_COUNT_2);
}
