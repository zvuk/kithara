use serde_yaml_ng::Value;

use super::{GitlabConfig, mapping, workspace_root};

#[test]
fn dependency_audit_publishes_raw_evidence_on_every_job_outcome() {
    let config = GitlabConfig::load(workspace_root());
    assert_eq!(
        config.effective_value("deps:deny", "script"),
        Some(Value::Sequence(vec![Value::String(
            "just ci run deps-deny".to_owned()
        )]))
    );
    let artifacts = config
        .effective_value("deps:deny", "artifacts")
        .expect("deps:deny must publish its raw evidence");
    let artifacts = mapping(&artifacts, "the dependency audit artifacts");
    assert_eq!(
        artifacts.get("when").and_then(Value::as_str),
        Some("always")
    );
    assert_eq!(
        artifacts.get("expire_in").and_then(Value::as_str),
        Some("30 days")
    );
    let paths = artifacts
        .get("paths")
        .and_then(Value::as_sequence)
        .expect("dependency evidence paths are a sequence");
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].as_str(), Some(".ci-artifacts/deps-deny/"));
    assert_eq!(
        paths[1].as_str(),
        Some(".ci-artifacts/cache/gitlab-$CI_JOB_ID-*.json")
    );
}

#[cfg(unix)]
mod recipe {
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Command, Stdio},
    };

    use anyhow::{Context, Result};
    use tempfile::TempDir;

    use super::workspace_root;

    fn write_executable(path: &Path, body: &str) -> Result<()> {
        fs::write(path, body)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn copied_recipe(stdout: &[u8], stderr: &[u8], tee_status: i32) -> Result<TempDir> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let bin = root.join("bin");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(root.join(".config/just"))?;
        fs::copy(
            workspace_root().join(".config/just/deps.just"),
            root.join(".config/just/deps.just"),
        )?;
        fs::write(
            root.join("justfile"),
            "mod deps \".config/just/deps.just\"\n",
        )?;
        fs::write(root.join("stdout.bin"), stdout)?;
        fs::write(root.join("stderr.bin"), stderr)?;
        write_executable(
            &bin.join("cargo"),
            r#"#!/bin/sh
set -eu
printf '%s\000' "$@" >> cargo-calls
printf '\n' >> cargo-calls
case "$*" in
  'fetch --locked') [ "$#" -eq 2 ] ;;
  'deny fetch db') [ "$#" -eq 3 ] ;;
  'deny --offline check --hide-inclusion-graph')
    [ "$#" -eq 4 ]
    cat stdout.bin
    cat stderr.bin >&2
    exit "$FAKE_CARGO_STATUS"
    ;;
  *) exit 97 ;;
esac
"#,
        )?;
        if tee_status != 0 {
            write_executable(
                &bin.join("tee"),
                r#"#!/bin/sh
set -eu
printf '%s\000' "$@" >> tee-calls
cat
exit 31
"#,
            )?;
        }
        Ok(temp)
    }

    #[test]
    fn dependency_audit_keeps_complete_raw_output_and_pipeline_status() -> Result<()> {
        let mut large_stdout = vec![b'x'; 16 * 1024 * 1024 + 1];
        large_stdout.extend_from_slice(b"\n\x1b[32mstdout tail\x1b[0m\0\xff\n");
        let diagnostics = b"\x1b[33mwarning: synthetic warning\x1b[0m\0\xfe\n\
            bug: synthetic diagnostic\n\
            advisories ok, bans ok, licenses ok, sources ok\n";
        let small_stdout = b"stdout\0\xff\n";
        let cases = [
            (0, 0, large_stdout.as_slice(), diagnostics.as_slice()),
            (23, 0, large_stdout.as_slice(), diagnostics.as_slice()),
            (0, 31, small_stdout.as_slice(), diagnostics.as_slice()),
            (23, 31, small_stdout.as_slice(), diagnostics.as_slice()),
            (0, 0, b"".as_slice(), b"".as_slice()),
        ];

        for (index, (cargo_status, tee_status, stdout, stderr)) in cases.into_iter().enumerate() {
            let temp = copied_recipe(stdout, stderr, tee_status)?;
            let root = temp.path();
            let evidence = root.join(".ci-artifacts/deps-deny");
            if index != 0 {
                fs::create_dir_all(&evidence)?;
                fs::write(evidence.join("cargo-deny.log"), b"stale raw evidence\n")?;
                fs::write(evidence.join("exit-status.txt"), b"stale status\n")?;
            }
            let mut paths = vec![root.join("bin")];
            if let Some(system_path) = env::var_os("PATH") {
                paths.extend(env::split_paths(&system_path));
            }
            let output = Command::new("just")
                .args(["--quiet", "--justfile"])
                .arg(root.join("justfile"))
                .args(["deps", "deny"])
                .current_dir(root)
                .env("PATH", env::join_paths(paths)?)
                .env("FAKE_CARGO_STATUS", cargo_status.to_string())
                .stdin(Stdio::null())
                .output()
                .context("run the copied dependency recipe through just")?;

            assert_eq!(
                fs::read(root.join("cargo-calls"))?,
                b"fetch\0--locked\0\ndeny\0fetch\0db\0\ndeny\0--offline\0check\0--hide-inclusion-graph\0\n",
                "the three Cargo argument lists must run once in order"
            );
            let raw = fs::read(evidence.join("cargo-deny.log"))
                .context("deps deny must create its raw artifact")?;
            let mut expected = stdout.to_vec();
            expected.extend_from_slice(stderr);
            assert!(
                output.stdout == expected,
                "case {index}: the recipe console must retain every merged diagnostic byte"
            );
            if tee_status == 0 {
                assert!(
                    raw == expected,
                    "case {index}: raw evidence must retain every byte and replace stale output"
                );
            } else {
                assert!(raw.is_empty(), "failed capture must not retain stale bytes");
                assert_eq!(
                    fs::read(root.join("tee-calls"))?,
                    b".ci-artifacts/deps-deny/cargo-deny.log\0",
                    "the failed capture must run once and drain the check output"
                );
            }
            assert_eq!(
                fs::read(evidence.join("exit-status.txt"))?,
                format!("cargo-deny={cargo_status}\ntee={tee_status}\n").as_bytes(),
                "case {index}: both original statuses must replace stale evidence"
            );
            let success = cargo_status == 0 && tee_status == 0 && !expected.is_empty();
            assert_eq!(
                output.status.success(),
                success,
                "case {index}: check failure, capture failure and empty evidence must fail just"
            );
            if success {
                assert!(
                    output.stderr.is_empty(),
                    "a successful recipe must stay quiet"
                );
            }
        }
        Ok(())
    }
}
