#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::json;

fn ready_stats() -> String {
    json!({"remote":"s3://scope/kache","daemon_connected":true,"daemon_remote_readonly":false})
        .to_string()
}

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write compiler-cache fixture executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("make compiler-cache fixture executable runnable");
}

#[test]
fn bootstrap_compiles_only_with_a_writable_s3_daemon() {
    let ready =
        json!({"remote":"s3://scope/kache","daemon_connected":true,"daemon_remote_readonly":false});
    for invalid in [
        None,
        Some(("remote", json!(null))),
        Some(("daemon_connected", json!(false))),
        Some(("daemon_remote_readonly", json!(true))),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut stats = ready.clone();
        if let Some((field, value)) = &invalid {
            stats[*field] = value.clone();
        }
        let kache = temp.path().join("kache");
        executable(
            &kache,
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'daemon start') exec \"$BOOTSTRAP_FAKE_KACHE\" --kache-start ;;\n'stats --json') printf '%s' '{}' ;;\n'daemon stop') test -d \"$KACHE_RUNTIME_DIR\" ;;\n*) exit 2 ;;\nesac\n",
                stats
            ),
        );
        executable(&temp.path().join("just"), "#!/bin/sh\nexit 0\n");
        let host = temp.path().join("host.toml");
        fs::write(&host, "[cache]\n").unwrap();
        let marker = temp.path().join("compiled");
        let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap")).args(["cache", "bootstrap"])
            .args(["sh", "-c", "test -z \"$GITHUB_ACTIONS$GITLAB_CI\" && test \"$RUSTC_WRAPPER\" = kache && test \"$KACHE_PREFETCH_ENABLED\" = false && touch \"$1\"", "compile"])
            .arg(&marker).env("PATH", path).env("TMPDIR", temp.path())
            .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
            .env("KITHARA_TEST_STDOUT", ready_stats())
            .env("CI_JOB_ID", "42").env("KITHARA_TEST_STDOUT", stats.to_string()).env_remove("GITHUB_RUN_ID")
            .env("GITHUB_ACTIONS", "true").env("GITLAB_CI", "true").env_remove("KACHE_RUNTIME_DIR")
            .env("KACHE_HOST_CONFIG", &host).env("SCCACHE_BUCKET", "scope")
            .env("SCCACHE_ENDPOINT", "http://cache:9000").output().unwrap();
        assert_eq!(
            output.status.success(),
            invalid.is_none(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(marker.exists(), invalid.is_none());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("kithara-compiler.")
        }));
    }
}

#[test]
fn child_failure_preserves_its_exit_and_stops_only_the_owned_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let stopped = temp.path().join("stopped");
    let host = temp.path().join("host.toml");
    fs::write(&host, "[cache]\n").unwrap();
    executable(&temp.path().join("just"), "#!/bin/sh\nexit 0\n");
    executable(
        &temp.path().join("kache"),
        &format!(
            "#!/bin/sh\ntest \"$KACHE_RUNTIME_DIR\" != /foreign || exit 9\ncase \"$1 $2\" in\n'daemon start') exec \"$BOOTSTRAP_FAKE_KACHE\" --kache-start ;;\n'stats --json') printf '%s' '{{\"remote\":\"s3://scope/kache\",\"daemon_connected\":true,\"daemon_remote_readonly\":false}}' ;;\n'daemon stop') printf '%s' \"$KACHE_RUNTIME_DIR\" > '{}' ;;\n*) exit 2 ;;\nesac\n",
            stopped.display(),
        ),
    );
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .args(["cache", "failed-child", "sh", "-c", "exit 7"])
        .env("PATH", path)
        .env("TMPDIR", temp.path())
        .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
        .env("KITHARA_TEST_STDOUT", ready_stats())
        .env("CI_JOB_ID", "42")
        .env("KACHE_RUNTIME_DIR", "/foreign")
        .env("KACHE_HOST_CONFIG", &host)
        .env("SCCACHE_BUCKET", "scope")
        .env("SCCACHE_ENDPOINT", "http://cache:9000")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = fs::read_to_string(stopped).unwrap();
    assert!(Path::new(&runtime).starts_with(temp.path()));
    assert!(!Path::new(&runtime).exists());
}

#[test]
fn a_quiet_phase_can_finish_after_its_daemon_expires() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host.toml");
    fs::write(&host, "[cache]\n").unwrap();
    executable(&temp.path().join("just"), "#!/bin/sh\nexit 0\n");
    executable(
        &temp.path().join("kache"),
        "#!/bin/sh\ncase \"$1 $2\" in\n'daemon start') rm -f \"$KACHE_RUNTIME_DIR/expired\"; exec \"$BOOTSTRAP_FAKE_KACHE\" --kache-start ;;\n'daemon stop') test ! -f \"$KACHE_RUNTIME_DIR/expired\" ;;\n*) exit 2 ;;\nesac\n",
    );
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .args([
            "cache",
            "quiet",
            "sh",
            "-c",
            "touch \"$KACHE_RUNTIME_DIR/expired\"",
        ])
        .env("PATH", path)
        .env("TMPDIR", temp.path())
        .env("CI_JOB_ID", "42")
        .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
        .env("KITHARA_TEST_STDOUT", ready_stats())
        .env("KACHE_HOST_CONFIG", host)
        .env("SCCACHE_BUCKET", "scope")
        .env("SCCACHE_ENDPOINT", "http://cache:9000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("kithara-compiler.")
    }));
}

#[test]
fn local_compilation_does_not_require_ci_services() {
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .args(["cache", "local", "sh", "-c", "exit 7"])
        .env_remove("CI_JOB_ID")
        .env_remove("GITHUB_RUN_ID")
        .env_remove("KACHE_HOST_CONFIG")
        .env_remove("SCCACHE_BUCKET")
        .env_remove("SCCACHE_ENDPOINT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
}

#[test]
fn a_failed_daemon_drain_fails_a_successful_compilation() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("host.toml");
    fs::write(&host, "[cache]\n").unwrap();
    executable(&temp.path().join("just"), "#!/bin/sh\nexit 0\n");
    executable(
        &temp.path().join("kache"),
        "#!/bin/sh\ncase \"$1 $2\" in\n'daemon start') exec \"$BOOTSTRAP_FAKE_KACHE\" --kache-start ;;\n'stats --json') printf '%s' '{\"remote\":\"s3://scope/kache\",\"daemon_connected\":true,\"daemon_remote_readonly\":false}' ;;\n'daemon stop') printf '%s' 'drain refused' >&2; exit 4 ;;\n*) exit 2 ;;\nesac\n",
    );
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .args(["cache", "drain", "sh", "-c", "exit 0"])
        .env("PATH", path)
        .env("TMPDIR", temp.path())
        .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
        .env("KITHARA_TEST_STDOUT", ready_stats())
        .env("CI_JOB_ID", "42")
        .env_remove("KACHE_RUNTIME_DIR")
        .env("KACHE_HOST_CONFIG", host)
        .env("SCCACHE_BUCKET", "scope")
        .env("SCCACHE_ENDPOINT", "http://cache:9000")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("drain refused"), "{stderr}");
    assert!(stderr.contains("exit status: 4"), "{stderr}");
    assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("kithara-compiler.")
    }));
}

#[test]
fn the_target_lease_preserves_the_ci_context_and_child_exit() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .arg("lease")
        .arg(&target)
        .args([
            "context",
            "sh",
            "-c",
            "test \"$GITHUB_ACTIONS\" = true && test \"$GITLAB_CI\" = true || exit 9; exit 7",
        ])
        .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
        .env("KITHARA_TEST_STDOUT", ready_stats())
        .env("CI_JOB_ID", "42")
        .env("GITHUB_RUN_ID", "43")
        .env("GITHUB_ACTIONS", "true")
        .env("GITLAB_CI", "true")
        .env_remove("KACHE_HOST_CONFIG")
        .env_remove("SCCACHE_BUCKET")
        .env_remove("SCCACHE_ENDPOINT")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(target.join(".kithara-job-lease").is_file());
    assert!(!target.join(".kithara-job-heartbeat").exists());
}

#[test]
fn the_initial_driver_daemon_is_verified_and_released_before_the_child() {
    for readonly in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("kithara-bootstrap.123456");
        fs::create_dir_all(&runtime).unwrap();
        let checkout = temp.path().join("deep-checkout-".repeat(12));
        fs::create_dir(&checkout).unwrap();
        let stopped = temp.path().join("stopped");
        let compiled = temp.path().join("compiled");
        let host = temp.path().join("host.toml");
        fs::write(&host, "[cache]\n").unwrap();
        let stats = json!({"remote":"s3://scope/kache","daemon_connected":true,"daemon_remote_readonly":readonly});
        executable(
            &temp.path().join("kache"),
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'daemon start') exec \"$BOOTSTRAP_FAKE_KACHE\" --kache-start ;;\n'stats --json') printf '%s' '{}' ;;\n'daemon stop') touch \"$BOOTSTRAP_STOPPED\" ;;\n*) exit 2 ;;\nesac\n",
                stats
            ),
        );
        executable(&temp.path().join("just"), "#!/bin/sh\nexit 0\n");
        let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
            .arg("--bootstrap-runtime")
            .arg(&runtime)
            .arg("lease")
            .arg(temp.path().join("target"))
            .args([
                "initial",
                "sh",
                "-c",
                "touch \"$BOOTSTRAP_COMPILED\"; exit 7",
            ])
            .current_dir(&checkout)
            .env("PATH", path)
            .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
            .env("KITHARA_TEST_STDOUT", ready_stats())
            .env("CI_JOB_ID", "42")
            .env("TMPDIR", temp.path())
            .env("KACHE_HOST_CONFIG", host)
            .env("SCCACHE_BUCKET", "scope")
            .env("SCCACHE_ENDPOINT", "http://cache:9000")
            .env("KITHARA_TEST_STDOUT", stats.to_string())
            .env("BOOTSTRAP_STOPPED", &stopped)
            .env("BOOTSTRAP_COMPILED", &compiled)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if readonly { 1 } else { 7 }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stopped.exists(),
            "the initial compilation daemon was not stopped"
        );
        assert!(
            !runtime.exists(),
            "the owned initial runtime was not removed"
        );
        assert_eq!(compiled.exists(), !readonly);
    }
}

#[test]
fn an_unused_local_startup_runtime_needs_no_compiler_cache_service() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("kithara-bootstrap.123456");
    fs::create_dir(&runtime).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
        .arg("--bootstrap-runtime")
        .arg(&runtime)
        .arg("lease")
        .arg(temp.path().join("target"))
        .args(["local", "sh", "-c", "exit 7"])
        .env("TMPDIR", temp.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CI_JOB_ID")
        .env_remove("GITHUB_RUN_ID")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!runtime.exists());
}

#[test]
fn the_initial_runtime_cannot_delete_other_data_or_follow_a_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let owned = temp.path().join("tmp");
    fs::create_dir(&owned).unwrap();
    let foreign = temp.path().join("foreign/kithara-bootstrap.123456");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("keep"), "keep").unwrap();
    let alias = owned.join("kithara-bootstrap.234567");
    std::os::unix::fs::symlink(&foreign, &alias).unwrap();
    let file = owned.join("kithara-bootstrap.345678");
    fs::write(&file, "keep").unwrap();
    let called = temp.path().join("called");
    executable(
        &temp.path().join("kache"),
        "#!/bin/sh\ntouch \"$BOOTSTRAP_CALLED\"\nexit 0\n",
    );
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    for runtime in [&foreign, &alias, &file] {
        let output = Command::new(env!("CARGO_BIN_EXE_ci-bootstrap"))
            .arg("--bootstrap-runtime")
            .arg(runtime)
            .args(["cache", "unsafe-runtime", "sh", "-c", "exit 0"])
            .current_dir(temp.path())
            .env("PATH", &path)
            .env("BOOTSTRAP_FAKE_KACHE", env!("CARGO_BIN_EXE_fake-tool"))
            .env("KITHARA_TEST_STDOUT", ready_stats())
            .env("CI_JOB_ID", "42")
            .env("TMPDIR", &owned)
            .env("BOOTSTRAP_CALLED", &called)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!called.exists());
        assert_eq!(fs::read_to_string(foreign.join("keep")).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
        assert!(alias.is_symlink());
    }
}
