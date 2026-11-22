//! The runner against `wasm-bindgen-test`'s own harness in each supported mode.
//!
//! The fixture binaries come from `kithara-wasm-runner-fixture`; the browser
//! lane builds them and names them in `KITHARA_WASM_RUNNER_FIXTURE_*`. A harness
//! whose page protocol moved away from the runner's fails here first.

use std::{env, process::Command};

use kithara_test_utils::kithara;

/// Arguments after the binary, whether the run passes, and the counts its
/// summary must report.
const CASES: [(&[&str], bool, &str); 4] = [
    (
        &["passes", "--exact"],
        true,
        "1 passed; 0 failed; 0 ignored",
    ),
    (
        &["fails", "--exact"],
        false,
        "0 passed; 1 failed; 0 ignored",
    ),
    (
        &["ignored", "--exact"],
        true,
        "0 passed; 0 failed; 1 ignored",
    ),
    (
        &["ignored", "--exact", "--include-ignored"],
        true,
        "1 passed; 0 failed; 0 ignored",
    ),
];

fn run(wasm: &str, args: &[String]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_wasm_test_runner"))
        .arg(wasm)
        .args(args)
        .output()
        .expect("the runner starts");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.success(),
        format!("{stdout}\n--- stderr ---\n{stderr}"),
    )
}

fn check(variable: &str, target: &str) {
    let wasm = env::var(variable)
        .unwrap_or_else(|_| panic!("{variable} must name the `{target}` fixture binary"));
    let (listed, output) = run(&wasm, &["--list".into()]);
    assert!(listed, "{output}");
    for name in ["passes", "fails", "ignored"] {
        assert!(output.contains(&format!("{name}: test")), "{output}");
    }
    for (args, passes, counts) in CASES {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let (passed, output) = run(&wasm, &args);
        assert_eq!(passed, passes, "{args:?}\n{output}");
        assert!(
            output.contains(counts),
            "{args:?} should report {counts}\n{output}"
        );
    }
}

/// One test, so the runs share the fixture server one at a time as cargo's
/// runs of real test binaries do.
#[kithara::test(native, timeout(::std::time::Duration::from_millis(33420)))]
fn each_mode_reports_pass_fail_and_ignored() {
    check("KITHARA_WASM_RUNNER_FIXTURE_BROWSER", "browser");
    check("KITHARA_WASM_RUNNER_FIXTURE_WORKER", "worker");
}
