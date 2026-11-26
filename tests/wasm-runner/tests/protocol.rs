//! The runner against `wasm-bindgen-test`'s own harness in each supported mode.
//!
//! The fixture binaries come from `kithara-wasm-runner-fixture`; the browser
//! lane builds them and names them in `KITHARA_WASM_RUNNER_FIXTURE_*`. A harness
//! whose page protocol moved away from the runner's fails here first.

use std::{
    env,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

use kithara_test_utils::kithara;

struct FixtureServer {
    child: Child,
    _stdout: BufReader<ChildStdout>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture_server() -> FixtureServer {
    let port = env::var("TEST_SERVER_PORT").unwrap_or_else(|_| "3444".into());
    let binary = Path::new(env!("CARGO_BIN_EXE_wasm_test_runner"))
        .parent()
        .expect("the runner has a parent directory")
        .join("test_server")
        .with_extension(env::consts::EXE_EXTENSION);
    let mut child = Command::new(binary)
        .env("TEST_SERVER_PORT", &port)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the shared fixture server starts");
    let mut stdout = BufReader::new(child.stdout.take().expect("fixture server stdout"));
    let mut record = String::new();
    let read = stdout
        .read_line(&mut record)
        .expect("fixture server readiness");
    assert!(read > 0, "fixture server exited before readiness");
    assert!(
        record.starts_with(&format!("test server listening on http://127.0.0.1:{port}")),
        "unexpected fixture server readiness record: {record}"
    );
    FixtureServer {
        child,
        _stdout: stdout,
    }
}

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
    eprintln!("wasm protocol child begin: {wasm} {args:?}");
    let output = Command::new(env!("CARGO_BIN_EXE_wasm_test_runner"))
        .arg(wasm)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the runner starts")
        .wait_with_output()
        .expect("the runner exits");
    eprintln!("wasm protocol child end: {} {args:?}", output.status);
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
#[kithara::test(native, timeout(Long))]
fn each_mode_reports_pass_fail_and_ignored() {
    let _fixture = fixture_server();
    check("KITHARA_WASM_RUNNER_FIXTURE_BROWSER", "browser");
    check("KITHARA_WASM_RUNNER_FIXTURE_WORKER", "worker");
}
