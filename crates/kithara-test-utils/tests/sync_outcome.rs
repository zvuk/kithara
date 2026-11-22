use kithara_test_utils::kithara;

type TestResult = Result<(), &'static str>;
type Unit = ();

#[kithara::test(native)]
#[kithara::hang_watchdog]
fn aliased_result() -> TestResult {
    hang_tick!();
    Ok(())
}

#[kithara::test(native)]
fn nested_result() -> Result<TestResult, &'static str> {
    Ok(Ok(()))
}

#[kithara::test(serial)]
#[cfg_attr(any(), should_panic)]
fn inactive_should_panic() -> TestResult {
    Ok(())
}

#[kithara::test(serial)]
#[cfg_attr(all(), should_panic(expected = "body panic"))]
fn active_should_panic_accepts_unit_alias() -> Unit {
    panic!("body panic");
}

#[kithara::test(native, serial, soft_fail("expected"))]
fn native_serial_soft_fail() {}

#[kithara::test(serial, soft_fail("expected"))]
fn dual_serial_soft_fail() {}

#[kithara::test(soft_fail("expected"))]
fn sync_soft_fail() {}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(native, browser, serial, soft_fail("expected"))]
fn browser_native_serial_soft_fail() {}
