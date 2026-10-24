#![cfg(test)]

#[cfg(all(feature = "multithread", not(target_arch = "wasm32")))]
use kithara_platform::tokio::task;
use kithara_test_utils::kithara;
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_utils::kithara_platform::tokio::runtime::{Handle, RuntimeFlavor};

#[kithara::test]
fn plain() {
    assert_eq!([20, 22].into_iter().sum::<u32>(), 42);
}

#[kithara::test]
#[case::zero(0, 0)]
#[case::positive(21, 42)]
fn parameterized(#[case] input: u32, #[case] expected: u32) {
    assert_eq!(input * 2, expected);
}

#[kithara::fixture]
fn seed() -> u32 {
    21
}

#[kithara::fixture]
fn doubled(seed: u32) -> u32 {
    seed * 2
}

#[kithara::test]
fn fixture_injection(doubled: u32) {
    assert_eq!(doubled, 42);
}

#[kithara::test(serial)]
fn serialized() {
    assert_eq!(doubled(), 42);
}

#[kithara::test(tokio)]
async fn current_thread() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        assert_eq!(
            Handle::current().runtime_flavor(),
            RuntimeFlavor::CurrentThread
        );
    }
    assert_eq!(std::future::ready(42).await, 42);
}

#[cfg(feature = "mock")]
mod mocking {
    use kithara_test_utils::{
        kithara,
        mock::{MockFn as _, Unimock, matching},
    };

    #[kithara::mock(api = ServiceMock)]
    pub trait Service {
        fn get(&self) -> u32;
    }

    #[kithara::test]
    fn mock_trait() {
        let service = Unimock::new(ServiceMock::get.next_call(matching!()).returns(42_u32));
        assert_eq!(service.get(), 42);
    }
}

#[cfg(all(feature = "multithread", not(target_arch = "wasm32")))]
#[kithara::test(tokio, multi_thread)]
async fn multithread_runtime() {
    assert_eq!(
        Handle::current().runtime_flavor(),
        RuntimeFlavor::MultiThread
    );
    assert_eq!(
        task::spawn(async { 42 }).await.expect("worker completed"),
        42
    );
}

#[cfg(all(feature = "hang", not(target_arch = "wasm32")))]
mod watchdog {
    use kithara_test_utils::kithara;

    #[kithara::hang_watchdog(name = "external-runtime-hang")]
    fn hang_without_progress() {
        hang_reset!();
        loop {
            hang_park!(std::thread::park_timeout);
        }
    }

    #[kithara::test(native, hang_timeout_secs(1), flash(false))]
    #[ignore = "negative watchdog case; run only in a bounded child process"]
    fn hang_watchdog_fires() {
        hang_without_progress();
    }
}

#[cfg(feature = "devtools")]
mod portable_config {
    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    #[derive(kithara_derive::Config)]
    #[config(default)]
    struct Settings {
        #[config(value, builder(default = 4), get(copy))]
        capacity: usize,
    }

    #[kithara::test]
    fn retained_config_and_derive() {
        assert_eq!(Settings::default().capacity(), 4);
        let settings = Settings::builder().capacity(8).build();
        assert_eq!(settings.capacity(), 8);
        assert_eq!(settings.values().capacity, 8);
    }
}
