#![cfg(feature = "cli")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .parent()
        .expect("xtask has a workspace root")
        .to_path_buf()
}

fn command(bin: &Path) -> Command {
    let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("prepend the test command directory to PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .current_dir(workspace_root())
        .env("PATH", path)
        .env_remove("KITHARA_XTASK_TARGET")
        .env_remove("CARGO_TARGET_DIR")
        .env("KITHARA_TEST_STDOUT", "stage stdout\n")
        .env("KITHARA_TEST_STDERR", "stage stderr\n");
    command
}

fn install_just(bin: &Path) {
    fs::copy(
        env!("CARGO_BIN_EXE_fake-tool"),
        bin.join(format!("just{}", std::env::consts::EXE_SUFFIX)),
    )
    .expect("install the existing executable test double as just");
}

#[test]
fn full_cycle_keeps_every_stage_receipt_and_rejects_a_failed_stage() {
    for (rules, accepted, exits) in [
        ("just:lint:*=7", false, ["7", "0", "0"]),
        ("just:test:*=9", false, ["0", "9", "9"]),
        ("", true, ["0", "0", "0"]),
    ] {
        let temp = tempfile::tempdir().unwrap();
        install_just(temp.path());
        let output = temp.path().join("cycle output ; literal");
        let trace = temp.path().join("commands");
        let result = command(temp.path())
            .args(["ci", "full"])
            .arg(&output)
            .env("KITHARA_TEST_TRACE", &trace)
            .env("KITHARA_TEST_RULES", rules)
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let commands = fs::read_to_string(trace).unwrap();
        assert_eq!(commands.lines().count(), 3, "{commands}");
        assert!(commands.starts_with("lint fast\ntest\ntest mutants run-all --output "));
        assert!(
            !fs::read_to_string(output.join("run-id"))
                .unwrap()
                .trim()
                .is_empty()
        );
        for (stage, exit) in ["lint", "test", "mutants"].into_iter().zip(exits) {
            assert_eq!(
                fs::read_to_string(output.join(format!("stage-{stage}.exit")))
                    .unwrap()
                    .trim(),
                exit
            );
            let log = fs::read_to_string(output.join(format!("stage-{stage}.log"))).unwrap();
            assert!(log.contains("stage stdout"), "{log}");
            assert!(log.contains("stage stderr"), "{log}");
        }
    }
}

#[test]
fn gate_stops_before_the_second_mode_when_the_first_fails() {
    let temp = tempfile::tempdir().unwrap();
    install_just(temp.path());
    let trace = temp.path().join("commands");
    let result = command(temp.path())
        .args(["ci", "gate", "--lane=engine-broadcast", "--retries=0"])
        .env("KITHARA_TEST_TRACE", &trace)
        .env("KITHARA_TEST_RULES", "just:test:*=7")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(trace).unwrap(),
        "test run --flash=on --no-block=on --lane=engine-broadcast --retries=0\n"
    );
}
