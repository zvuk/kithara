#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::thread::*;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use crate::native::thread::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::thread::*;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn native_thread_detectors_are_consistent() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(is_main_thread());
            assert!(!is_worker_thread());
            assert_main_thread("native-main");
            assert_not_main_thread("native-main");
        }
    }

    // `flash(false)`: a real park/unpark timing test. It measures REAL
    // wall-clock with `std::time::Instant` (not the platform clock) and asserts
    // the unpark wakes within 250ms. The lexical flash rewrite would retarget
    // `Instant::now()` onto the engine `virtual_now`, changing the clock
    // and leaving the `std::time::Instant` import unused, so opt out.
    #[kithara::test(flash(false))]
    fn park_timeout_returns_after_unpark() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let parked = current();
            let start = Instant::now();
            let join = spawn(move || {
                sleep(Duration::from_millis(5));
                parked.unpark();
            });
            park_timeout(Duration::from_secs(1));
            join.join()
                .expect("BUG: wake-helper thread joined cleanly without panicking");
            assert!(start.elapsed() < Duration::from_millis(250));
        }
    }
}
