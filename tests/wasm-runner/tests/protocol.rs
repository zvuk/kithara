//! The runner against `wasm-bindgen-test`'s own harness in each supported mode.
//!
//! The fixture binaries come from `kithara-wasm-runner-fixture`; the browser
//! lane builds them and names them in `KITHARA_WASM_RUNNER_FIXTURE_*`. A harness
//! whose page protocol moved away from the runner's fails here first.

use std::{
    env,
    process::{Command, Stdio},
};

use kithara_test_utils::kithara;

const BROWSER: &str = "KITHARA_WASM_RUNNER_FIXTURE_BROWSER";
const WORKER: &str = "KITHARA_WASM_RUNNER_FIXTURE_WORKER";
const LISTED: &[&str] = &["passes: test", "fails: test", "ignored: test"];

/// One runner invocation per case. Cases run one at a time, as cargo's runs of
/// real test binaries do, because every run shares the fixture server port.
#[kithara::test(native, serial, timeout(Heavy))]
#[case::browser_list(BROWSER, &["--list"], true, LISTED)]
#[case::browser_passes(BROWSER, &["passes", "--exact"], true, &["1 passed; 0 failed; 0 ignored"])]
#[case::browser_fails(BROWSER, &["fails", "--exact"], false, &["0 passed; 1 failed; 0 ignored"])]
#[case::browser_ignored(BROWSER, &["ignored", "--exact"], true, &["0 passed; 0 failed; 1 ignored"])]
#[case::browser_include_ignored(
    BROWSER,
    &["ignored", "--exact", "--include-ignored"],
    true,
    &["1 passed; 0 failed; 0 ignored"]
)]
#[case::worker_list(WORKER, &["--list"], true, LISTED)]
#[case::worker_passes(WORKER, &["passes", "--exact"], true, &["1 passed; 0 failed; 0 ignored"])]
#[case::worker_fails(WORKER, &["fails", "--exact"], false, &["0 passed; 1 failed; 0 ignored"])]
#[case::worker_ignored(WORKER, &["ignored", "--exact"], true, &["0 passed; 0 failed; 1 ignored"])]
#[case::worker_include_ignored(
    WORKER,
    &["ignored", "--exact", "--include-ignored"],
    true,
    &["1 passed; 0 failed; 0 ignored"]
)]
fn the_runner_reports_the_harness_outcome(
    #[case] variable: &str,
    #[case] args: &[&str],
    #[case] passes: bool,
    #[case] reports: &[&str],
) {
    let wasm =
        env::var(variable).unwrap_or_else(|_| panic!("{variable} must name a fixture binary"));
    let output = Command::new(env!("CARGO_BIN_EXE_wasm_test_runner"))
        .arg(&wasm)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("the runner runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.success(), passes, "{args:?}\n{stdout}");
    for report in reports {
        assert!(
            stdout.contains(report),
            "{args:?} should report {report}\n{stdout}"
        );
    }
}
