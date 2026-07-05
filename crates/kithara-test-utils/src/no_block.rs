use std::future::Future;

pub use kithara_platform::no_block::{Pause, Permit, PermitPoll, Watched, permit, permit_poll};

#[doc(hidden)]
#[track_caller]
pub fn watch<F: Future>(name: &'static str, budget_ms: u64, fut: F) -> Watched<F> {
    kithara_platform::no_block::watch_budget(name, budget_ms, fut)
}

#[doc(hidden)]
#[track_caller]
pub fn watch_root<F: Future>(name: &'static str, fut: F) -> Watched<F> {
    kithara_platform::no_block::watch_blanket(name, fut)
}

#[cfg(test)]
mod tests {
    use std::{
        any::Any,
        future::Future,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::Once,
    };

    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    const BUDGET_MS: u64 = 5;
    const SLEEP_MS: u64 = 1;
    const SPIN_MS: u64 = 50;

    static PANIC_MODE: Once = Once::new();

    fn force_panic_mode() {
        PANIC_MODE.call_once(|| {
            // SAFETY: these tests set one process-global mode to the same value before
            // any no_block-watched poll runs in this test binary.
            unsafe { std::env::set_var("KITHARA_NO_BLOCK", "panic") };
        });
    }

    fn run<F: Future<Output = ()>>(fut: F) {
        let rt = kithara_platform::tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        rt.block_on(fut);
    }

    fn spin_for(duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            std::hint::spin_loop();
        }
    }

    fn panic_message(err: &(dyn Any + Send)) -> &str {
        err.downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&'static str>().copied())
            .unwrap_or("")
    }

    #[kithara::no_block(budget_ms = 5)]
    async fn no_block_spin_panics() {
        spin_for(Duration::from_millis(SPIN_MS));
    }

    #[kithara::allow_block]
    fn sync_allowed_sleep() {
        kithara_platform::thread::sleep(Duration::from_millis(SLEEP_MS));
    }

    #[kithara::no_block(budget_ms = 10_000)]
    async fn sync_allow_block_passes() {
        sync_allowed_sleep();
    }

    #[kithara::allow_block]
    async fn async_allowed_sleep() {
        kithara_platform::thread::sleep(Duration::from_millis(SLEEP_MS));
    }

    #[kithara::no_block(budget_ms = 10_000)]
    async fn async_allow_block_passes() {
        async_allowed_sleep().await;
    }

    #[test]
    fn no_block_attr_panics_with_fn_path() {
        force_panic_mode();

        let err = catch_unwind(AssertUnwindSafe(|| run(no_block_spin_panics())))
            .expect_err("over-budget no_block async fn must panic");
        let msg = panic_message(err.as_ref());
        assert!(
            msg.contains(concat!(module_path!(), "::no_block_spin_panics")),
            "got: {msg}"
        );
        assert!(msg.contains(&format!("budget {BUDGET_MS}ms")), "got: {msg}");
    }

    #[test]
    fn allow_block_sync_fn_suppresses_forbid() {
        force_panic_mode();

        let caught = catch_unwind(AssertUnwindSafe(|| run(sync_allow_block_passes())));
        assert!(caught.is_ok());
    }

    #[test]
    fn allow_block_async_fn_suppresses_forbid() {
        force_panic_mode();

        let caught = catch_unwind(AssertUnwindSafe(|| run(async_allow_block_passes())));
        assert!(caught.is_ok());
    }
}
