/// Wrap a `loop` or `while` with a [`HangDetector`](crate::HangDetector).
///
/// Inside the loop body, two helper macros are available:
/// - `hang_tick!()` — advance the detector's tick counter.
/// - `hang_reset!()` — reset the detector (call when progress is made).
///
/// # Syntax
///
/// ```text
/// hang_watchdog! {
///     [thread: <expr>;]      // optional: detector label (default: module_path!:line!)
///     [timeout: <expr>;]     // optional: hang timeout  (default: default_timeout())
///     loop { ... }           // or: while <condition> { ... }
/// }
/// ```
///
/// `thread:` and `timeout:` may appear in either order.
#[macro_export]
macro_rules! hang_watchdog {
    // Internal rule — preamble written once.
    (@inner name=$name:expr, timeout=$timeout:expr; $($loop_body:tt)*) => {{
        let mut __hang_detector = $crate::HangDetector::new($name, $timeout);
        #[allow(unused_macros)]
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        #[allow(unused_macros)]
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        $($loop_body)*
    }};

    // --- public entry points (most specific first) ---

    (thread: $thread:expr; timeout: $timeout:expr; $($loop_body:tt)*) => {
        $crate::hang_watchdog!(@inner name=$thread, timeout=$timeout; $($loop_body)*)
    };
    (timeout: $timeout:expr; thread: $thread:expr; $($loop_body:tt)*) => {
        $crate::hang_watchdog!(@inner name=$thread, timeout=$timeout; $($loop_body)*)
    };
    (thread: $thread:expr; $($loop_body:tt)*) => {
        $crate::hang_watchdog!(@inner name=$thread, timeout=$crate::default_timeout(); $($loop_body)*)
    };
    (timeout: $timeout:expr; $($loop_body:tt)*) => {
        $crate::hang_watchdog!(@inner name=concat!(module_path!(), ":", line!()), timeout=$timeout; $($loop_body)*)
    };
    ($($loop_body:tt)*) => {
        $crate::hang_watchdog!(@inner name=concat!(module_path!(), ":", line!()), timeout=$crate::default_timeout(); $($loop_body)*)
    };
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn macro_loop_without_options_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            loop {
                count += 1;
                if count >= 3 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn macro_while_without_options_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            while count < 3 {
                count += 1;
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn macro_with_thread_option_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            thread: "test.thread";
            loop {
                count += 1;
                if count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn macro_with_thread_option_and_while_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            thread: "test.thread";
            while count < 2 {
                count += 1;
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn macro_with_timeout_option_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            timeout: Duration::from_secs(1);
            loop {
                count += 1;
                if count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn macro_with_thread_and_timeout_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            thread: "test.thread";
            timeout: Duration::from_secs(1);
            loop {
                count += 1;
                if count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn macro_with_timeout_and_thread_compiles_and_runs() {
        let mut count = 0;
        crate::hang_watchdog! {
            timeout: Duration::from_secs(1);
            thread: "test.thread";
            loop {
                count += 1;
                if count >= 2 {
                    break;
                }
                hang_reset!();
                hang_tick!();
            }
        }
        assert_eq!(count, 2);
    }
}
