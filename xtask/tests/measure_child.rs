#![cfg(unix)]

use std::{
    env,
    ffi::OsString,
    os::unix::{ffi::OsStringExt as _, process::ExitStatusExt as _},
    process::{Command, Output},
};

use serde_json::Value;

fn observer() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.args(["measure-child", "--"]).current_dir("/");
    command
}

fn metrics(output: &Output) -> Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rows: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("KITHARA_NATIVE_TIMING "))
        .collect();
    assert_eq!(rows.len(), 1, "{stderr}");
    let value: Value = serde_json::from_str(rows[0]).expect("native timing JSON");
    assert!(value["child_pid"].as_u64().expect("native child PID") > 0);
    for field in ["wall_seconds", "user_seconds", "system_seconds"] {
        let number = value[field].as_f64().expect("numeric duration");
        assert!(number.is_finite() && number >= 0.0, "{value}");
    }
    value
}

#[test]
fn environment_arguments_and_process_group_are_preserved() {
    const FIXTURE: &str = "KITHARA_MEASURE_CHILD_ENV_FIXTURE";
    if env::var_os(FIXTURE).is_some() {
        assert_eq!(
            env::var_os("DYLD_LIBRARY_PATH"),
            env::var_os("KITHARA_EXPECT_DYLD")
        );
        assert!(env::args_os().any(|arg| arg == "argument with spaces"));
        assert_eq!(
            nix::unistd::getpgrp().to_string(),
            env::var("KITHARA_EXPECT_GROUP").expect("original process group")
        );
        println!("measured-child-output");
        eprintln!("measured-child-error");
        return;
    }
    let mut dyld = env::var_os("DYLD_LIBRARY_PATH").unwrap_or_default();
    dyld.push(":kithara-native-child-sentinel");
    let output = observer()
        .arg(env::current_exe().expect("integration test executable"))
        .args([
            "--exact",
            "environment_arguments_and_process_group_are_preserved",
            "--nocapture",
            "--skip",
            "argument with spaces",
        ])
        .env(FIXTURE, "1")
        .env("DYLD_LIBRARY_PATH", &dyld)
        .env("KITHARA_EXPECT_DYLD", &dyld)
        .env("KITHARA_EXPECT_GROUP", nix::unistd::getpgrp().to_string())
        .output()
        .expect("run native observer");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("measured-child-output"), "{stdout}");
    assert!(stdout.contains("test result: ok. 1 passed;"), "{stdout}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("measured-child-error"));
    metrics(&output);
}

#[test]
fn nonzero_exit_and_both_output_streams_are_preserved() {
    let output = observer()
        .arg(env!("CARGO_BIN_EXE_fake-tool"))
        .env("KITHARA_TEST_RULES", "*:*:*=7")
        .env("KITHARA_TEST_STDOUT", "original stdout")
        .env("KITHARA_TEST_STDERR", "original stderr\n")
        .output()
        .expect("run failing measured child");
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(output.stdout, b"original stdout");
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("original stderr\n"));
    metrics(&output);
}

#[test]
fn child_signals_remain_signals() {
    for (name, number) in [("TERM", 15), ("KILL", 9), ("PIPE", 13)] {
        let output = observer()
            .args(["/bin/sh", "-c", &format!("kill -{name} $$")])
            .output()
            .expect("run signalled measured child");
        assert_eq!(output.status.signal(), Some(number), "{name}: {output:?}");
        metrics(&output);
    }
}

#[test]
fn native_cpu_accounts_for_actual_child_work() {
    const FIXTURE: &str = "KITHARA_MEASURE_CHILD_CPU_FIXTURE";
    if env::var_os(FIXTURE).is_some() {
        let mut result = 0u64;
        // Keep the work above millisecond-scale native CPU accounting resolution.
        for value in 0..64_000_000u64 {
            result = std::hint::black_box(result.wrapping_add(std::hint::black_box(value)));
        }
        assert_eq!(result, 2_047_999_968_000_000);
        println!("measured-child-pid={}", std::process::id());
        return;
    }
    let output = observer()
        .arg(env::current_exe().expect("integration test executable"))
        .args([
            "--exact",
            "native_cpu_accounts_for_actual_child_work",
            "--nocapture",
        ])
        .env(FIXTURE, "1")
        .output()
        .expect("run CPU child");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    let value = metrics(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .lines()
        .find_map(|line| line.strip_prefix("measured-child-pid="))
        .expect("actual CPU child PID")
        .parse::<u64>()
        .expect("numeric CPU child PID");
    assert_eq!(value["child_pid"].as_u64(), Some(pid));
    assert!(
        value["user_seconds"].as_f64().expect("user CPU")
            + value["system_seconds"].as_f64().expect("system CPU")
            > 0.001,
        "{value}"
    );
}

#[test]
fn non_utf8_arguments_reach_the_child_unchanged() {
    let argument = vec![0xff, b' ', b'x'];
    let output = observer()
        .args(["/bin/sh", "-c", "printf '%s' \"$1\"", "child"])
        .arg(OsString::from_vec(argument.clone()))
        .output()
        .expect("run byte argument child");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, argument);
    metrics(&output);
}

#[test]
fn a_missing_command_cannot_report_success() {
    let output = observer()
        .arg("/kithara-no-such-measured-command")
        .output()
        .expect("run missing child command");
    assert!(!output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("KITHARA_NATIVE_TIMING "));
}
