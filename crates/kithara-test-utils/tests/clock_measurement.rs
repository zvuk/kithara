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

#[kithara::test(native, browser)]
fn browser_native_sync() {
    platform::thread::sleep(STEP);
}

#[kithara::test(tokio, timeout(::std::time::Duration::from_millis(1152)))]
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
    let identity = format!("{{\"test\":\"{}::{name}\",\"pid\":{pid},", module_path!());
    let report = reports[0]
        .strip_prefix(&identity)
        .expect("selected test and actual child PID");
    let expected = format!("\"mode\":\"{mode}\",\"elapsed_ns\":");
    let elapsed = report
        .strip_prefix(&expected)
        .and_then(|tail| tail.strip_suffix('}'))
        .expect("actual clock mode and numeric elapsed")
        .parse::<u128>()
        .expect("elapsed nanoseconds");
    if name == "changed_clock" && cfg!(feature = "flash") {
        assert_eq!(elapsed, 0, "the original virtual clock did not advance");
    } else if name != "changed_clock" {
        assert!(elapsed >= STEP.as_nanos(), "{reports:?}");
    }
}
