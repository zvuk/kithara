#[macro_export]
macro_rules! hang_watchdog {
    (loop $body:block) => {{
        let mut __hang_detector = $crate::HangDetector::new(
            concat!(module_path!(), ":", line!()),
            $crate::default_timeout(),
        );
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        loop $body
    }};
    (thread: $thread:expr; loop $body:block) => {{
        let mut __hang_detector = $crate::HangDetector::new($thread, $crate::default_timeout());
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        loop $body
    }};
    (timeout: $timeout:expr; loop $body:block) => {{
        let mut __hang_detector =
            $crate::HangDetector::new(concat!(module_path!(), ":", line!()), $timeout);
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        loop $body
    }};
    (thread: $thread:expr; timeout: $timeout:expr; loop $body:block) => {{
        let mut __hang_detector = $crate::HangDetector::new($thread, $timeout);
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        loop $body
    }};
    (timeout: $timeout:expr; thread: $thread:expr; loop $body:block) => {{
        let mut __hang_detector = $crate::HangDetector::new($thread, $timeout);
        macro_rules! hang_tick {
            () => {
                __hang_detector.tick();
            };
        }
        macro_rules! hang_reset {
            () => {
                __hang_detector.reset();
            };
        }
        loop $body
    }};
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
