mod detector;

pub use detector::{HangDetector, default_timeout};
pub use kithara_hang_detector_macros::hang_watchdog;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn attr_macro_loop_compiles_and_runs() {
        let mut count = 0;

        #[kithara_hang_detector::hang_watchdog]
        fn run_loop(count: &mut i32) {
            loop {
                *count += 1;
                if *count >= 3 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }

        run_loop(&mut count);
        assert_eq!(count, 3);
    }

    #[test]
    fn attr_macro_while_compiles_and_runs() {
        let mut count = 0;

        #[kithara_hang_detector::hang_watchdog]
        fn run_while(count: &mut i32) {
            while *count < 3 {
                *count += 1;
                hang_reset!();
                hang_tick!();
            }
        }

        run_while(&mut count);
        assert_eq!(count, 3);
    }

    #[test]
    fn attr_macro_with_thread_compiles_and_runs() {
        let mut count = 0;

        #[kithara_hang_detector::hang_watchdog(name = "test.thread")]
        fn run_loop(count: &mut i32) {
            loop {
                *count += 1;
                if *count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }

        run_loop(&mut count);
        assert_eq!(count, 2);
    }

    #[test]
    fn attr_macro_with_timeout_compiles_and_runs() {
        let mut count = 0;

        #[kithara_hang_detector::hang_watchdog(timeout = Duration::from_secs(1))]
        fn run_loop(count: &mut i32) {
            loop {
                *count += 1;
                if *count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }

        run_loop(&mut count);
        assert_eq!(count, 2);
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
                if *count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }

        run_loop(&mut count);
        assert_eq!(count, 2);
    }
}
