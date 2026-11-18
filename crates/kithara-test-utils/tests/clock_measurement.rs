#![cfg(not(target_arch = "wasm32"))]

use std::{
    env,
    process::{Command, Stdio},
};

use kithara_test_utils::{
    kithara,
    kithara_platform::{self as platform, time::Duration},
};

const REPORT_ENV: &str = "KITHARA_TEST_CLOCK_MEASUREMENT";
const REPORT_PREFIX: &str = "KITHARA_TEST_CLOCK ";
const STEP: Duration = Duration::from_millis(7);

#[kithara::test]
fn plain_sync() {
    platform::thread::sleep(STEP);
}

#[kithara::test(native)]
fn sync_timeout() {
    platform::thread::sleep(STEP);
}

#[kithara::test(native)]
fn sync_timeout_accepts_a_non_send_error() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[kithara::test(native)]
#[cfg_attr(any(), should_panic)]
fn sync_inactive_should_panic_accepts_a_non_send_error() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[kithara::test(native, serial)]
fn sync_serial_accepts_a_non_send_error() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[kithara::test(native, soft_fail("expected"))]
fn sync_soft_fail_keeps_a_unit_return() {}

#[kithara::test(native, soft_fail("expected"))]
fn sync_soft_fail_matched_panic_passes() {
    panic!("expected panic");
}

#[kithara::test(native)]
#[ignore]
fn sync_non_send_err_is_a_failure() -> Result<(), Box<dyn std::error::Error>> {
    Err(std::io::Error::other("expected failure").into())
}

#[kithara::test(native, serial)]
#[ignore]
fn sync_serial_err_is_a_failure() -> Result<(), Box<dyn std::error::Error>> {
    Err(std::io::Error::other("expected failure").into())
}

#[kithara::test(native, soft_fail("failure"))]
#[ignore]
fn sync_soft_fail_result_err_is_a_failure() -> Result<(), Box<dyn std::error::Error>> {
    Err(std::io::Error::other("matching body failure").into())
}

#[kithara::test(native, soft_fail("expected"))]
#[ignore]
fn sync_soft_fail_unexpected_panic_fails() {
    panic!("boom");
}

#[kithara::test(native, browser)]
fn browser_native_sync() {
    platform::thread::sleep(STEP);
}

#[kithara::test(tokio)]
async fn async_early_result() -> Result<(), &'static str> {
    let started = platform::time::Instant::now();
    platform::time::sleep(STEP).await;
    if started.elapsed() >= STEP {
        return Ok(());
    }
    Err("body clock did not advance")
}

#[kithara::test(native, tokio)]
async fn async_timeout() {
    platform::time::sleep(STEP).await;
}

#[kithara::test(native, tokio)]
#[should_panic(expected = "expected clock panic")]
async fn async_expected_panic() {
    platform::time::sleep(STEP).await;
    panic!("expected clock panic");
}

#[kithara::test(native, flash(false))]
fn real_sync() {
    platform::thread::sleep(STEP);
}

#[kithara::test(native, tokio, flash(false))]
async fn real_async() {
    platform::time::sleep(STEP).await;
}

#[kithara::flash(true)]
fn clock_entry() -> Option<kithara_test_utils::test::ClockMeasurement> {
    kithara_test_utils::test::ClockMeasurement::start(
        concat!(module_path!(), "::changed_clock"),
        true,
    )
}

#[test]
fn changed_clock() {
    let measurement = {
        let _ambient = platform::flash::ambient_scope(true);
        clock_entry()
    };
    drop(measurement);
}

#[kithara::test(native)]
#[case::plain("plain_sync", "flash")]
#[case::sync_timeout("sync_timeout", "flash")]
#[case::browser_native("browser_native_sync", "flash")]
#[case::async_result("async_early_result", "flash")]
#[case::async_timeout("async_timeout", "flash")]
#[case::expected_panic("async_expected_panic", "flash")]
#[case::real_sync("real_sync", "real")]
#[case::real_async("real_async", "real")]
#[case::changed("changed_clock", "flash")]
#[case::disabled("plain_sync", "disabled")]
fn reports_the_actual_body_clock(#[case] name: &str, #[case] expected_mode: &str) {
    let mut command = Command::new(env::current_exe().expect("clock control executable"));
    command.args(["--exact", name, "--nocapture"]);
    if expected_mode == "disabled" {
        command.env_remove(REPORT_ENV);
    } else {
        command.env(REPORT_ENV, "1");
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run clock control");
    let pid = child.id();
    let output = child.wait_with_output().expect("wait for clock control");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reports: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(REPORT_PREFIX))
        .collect();
    if expected_mode == "disabled" {
        assert!(reports.is_empty(), "{stderr}");
        return;
    }
    assert_eq!(
        reports.len(),
        1,
        "one actual clock report required: {stderr}"
    );
    let mode = if !cfg!(feature = "flash") {
        "real"
    } else {
        expected_mode
    };
    let report: serde_json::Value = serde_json::from_str(reports[0]).expect("clock report JSON");
    assert_eq!(report["test"], format!("{}::{name}", module_path!()));
    assert_eq!(report["pid"].as_u64(), Some(u64::from(pid)));
    assert_eq!(report["mode"], mode);
    let elapsed = u128::from(report["elapsed_ns"].as_u64().expect("elapsed nanoseconds"));
    let wall_elapsed = u128::from(
        report["wall_elapsed_ns"]
            .as_u64()
            .expect("wall elapsed nanoseconds"),
    );
    if matches!(name, "real_sync" | "real_async") {
        assert!(wall_elapsed >= STEP.as_nanos(), "{reports:?}");
    }
    if name == "changed_clock" && cfg!(feature = "flash") {
        assert_eq!(elapsed, 0, "the original virtual clock did not advance");
    } else if name != "changed_clock" {
        assert!(elapsed >= STEP.as_nanos(), "{reports:?}");
    }
}

#[test]
fn sync_body_failures_report_a_failed_status() {
    for (name, diagnostic) in [
        ("sync_non_send_err_is_a_failure", "expected failure"),
        ("sync_serial_err_is_a_failure", "expected failure"),
        (
            "sync_soft_fail_result_err_is_a_failure",
            "matching body failure",
        ),
        ("sync_soft_fail_unexpected_panic_fails", "boom"),
    ] {
        let output = Command::new(env::current_exe().expect("clock control executable"))
            .args(["--exact", name, "--ignored"])
            .output()
            .expect("run sync failure probe");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(diagnostic),
            "{output:?}"
        );
        assert!(
            !output.status.success(),
            "{name} must report a failed status, got {output:?}"
        );
    }
}
