use std::future::Future;

pub use kithara_platform::no_block::{Pause, Watched};
#[cfg(not(rtsan))]
pub use kithara_platform::no_block::{Permit, PermitPoll, permit, permit_poll};

#[cfg(rtsan)]
mod rtsan_gate {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use pin_project_lite::pin_project;

    #[cfg(feature = "no-block")]
    pin_project! {
        pub struct RtsanChecked<F> {
            #[pin]
            pub(super) fut: F,
        }
    }

    pin_project! {
        pub struct PermitPoll<F> {
            #[pin]
            pub(super) fut: F,
        }
    }

    #[cfg(feature = "no-block")]
    impl<F: Future> Future for RtsanChecked<F> {
        type Output = F::Output;

        #[sanitize(realtime = "nonblocking")]
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.project().fut.poll(cx)
        }
    }

    impl<F: Future> Future for PermitPoll<F> {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.project();
            let _permit = super::permit();
            this.fut.poll(cx)
        }
    }
}

#[cfg(rtsan)]
pub use rtsan_gate::PermitPoll;

#[cfg(rtsan)]
#[must_use = "the permit only suspends no_block and RTSan checks while the guard is alive"]
pub struct Permit {
    _platform: kithara_platform::no_block::Permit,
    _rtsan: crate::rtsan::Permit,
}

#[cfg(rtsan)]
pub fn permit() -> Permit {
    Permit {
        _platform: kithara_platform::no_block::permit(),
        _rtsan: crate::rtsan::permit(),
    }
}

#[cfg(rtsan)]
#[doc(hidden)]
#[must_use]
pub fn permit_poll<F: Future>(fut: F) -> PermitPoll<F> {
    PermitPoll { fut }
}

#[doc(hidden)]
#[track_caller]
#[cfg(not(all(rtsan, feature = "no-block")))]
pub fn watch<F: Future>(name: &'static str, budget_ms: u64, fut: F) -> Watched<F> {
    kithara_platform::no_block::watch_budget(name, budget_ms, fut)
}

#[doc(hidden)]
#[track_caller]
#[cfg(all(rtsan, feature = "no-block"))]
pub fn watch<F: Future>(
    name: &'static str,
    budget_ms: u64,
    fut: F,
) -> Watched<rtsan_gate::RtsanChecked<F>> {
    kithara_platform::no_block::watch_budget(name, budget_ms, rtsan_gate::RtsanChecked { fut })
}

#[doc(hidden)]
#[track_caller]
#[cfg(not(all(rtsan, feature = "no-block")))]
pub fn watch_root<F: Future>(name: &'static str, fut: F) -> Watched<F> {
    kithara_platform::no_block::watch_blanket(name, fut)
}

#[doc(hidden)]
#[track_caller]
#[cfg(all(rtsan, feature = "no-block"))]
pub fn watch_root<F: Future>(name: &'static str, fut: F) -> Watched<rtsan_gate::RtsanChecked<F>> {
    kithara_platform::no_block::watch_blanket(name, rtsan_gate::RtsanChecked { fut })
}

#[cfg(all(test, feature = "no-block"))]
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

    #[kithara::test]
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

    #[kithara::test]
    fn allow_block_sync_fn_suppresses_forbid() {
        force_panic_mode();

        let caught = catch_unwind(AssertUnwindSafe(|| run(sync_allow_block_passes())));
        assert!(caught.is_ok());
    }

    #[kithara::test]
    fn allow_block_async_fn_suppresses_forbid() {
        force_panic_mode();

        let caught = catch_unwind(AssertUnwindSafe(|| run(async_allow_block_passes())));
        assert!(caught.is_ok());
    }
}
