#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::json;

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
                "#!/bin/sh\ncase \"$1 $2\" in\n'stats --json') printf '%s' '{}' ;;\n'daemon stop') test -d \"$KACHE_RUNTIME_DIR\" ;;\n*) exit 2 ;;\nesac\n",
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
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".config/ci/compiler-cache.sh");
        let output = Command::new("bash").arg(script).arg("bootstrap")
            .args(["sh", "-c", "test -z \"$GITHUB_ACTIONS$GITLAB_CI\" && test \"$RUSTC_WRAPPER\" = kache && touch \"$1\"", "compile"])
            .arg(&marker).env("PATH", path).env("TMPDIR", temp.path())
            .env("CI_JOB_ID", "42").env_remove("GITHUB_RUN_ID")
            .env("GITHUB_ACTIONS", "true").env("GITLAB_CI", "true")
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
