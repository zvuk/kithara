use std::{env, process::Command};

use anyhow::{Context, Result};
use tempfile::TempDir;

use crate::ci::{config::CiPins, process::Process};

pub(super) fn prepare(
    process: &Process,
    pins: &CiPins,
    cargo: &str,
    arguments: &[String],
    command: &mut Command,
) -> Result<Option<TempDir>> {
    if !arguments
        .iter()
        .map(String::as_str)
        .eq(["perf", "bench", "ci"])
        || process.environment_var("RUN_BENCHMARKS").as_deref() != Some("1".as_ref())
        || process
            .environment_var("BENCH_BASE_REF")
            .is_none_or(|value| value.is_empty())
    {
        return Ok(None);
    }
    let tool = install(process, pins, cargo)?;
    super::nextest::prepend(command, &tool.path().join("bin"))?;
    Ok(Some(tool))
}

fn install(process: &Process, pins: &CiPins, cargo: &str) -> Result<TempDir> {
    let scratch = process
        .environment_path("TMPDIR")
        .unwrap_or_else(env::temp_dir);
    let root = tempfile::tempdir_in(scratch).context("prepare the job's critcmp tool")?;
    process.run_command(
        process
            .command(cargo)
            .args([
                "install",
                "--locked",
                "--version",
                pins.cargo_tool_version("critcmp")?,
                "--root",
            ])
            .arg(root.path())
            .arg("critcmp")
            .current_dir(root.path())
            .env("CARGO_TARGET_DIR", root.path().join("build"))
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTDOCFLAGS")
            .env_remove("CARGO_BUILD_TARGET"),
        "install the job's pinned critcmp",
    )?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use super::*;
    use crate::ci::{config::fixture, process::Recording};

    #[test]
    fn head_only_and_disabled_benchmarks_need_no_comparison_tool() {
        for (run, base) in [("0", "main"), ("1", "")] {
            let process = Process::new(
                Path::new("/checkout"),
                BTreeMap::from([
                    ("RUN_BENCHMARKS".into(), run.into()),
                    ("BENCH_BASE_REF".into(), base.into()),
                ]),
            );
            let mut command = process.command("just");
            assert!(
                prepare(
                    &process,
                    &fixture().pins,
                    "missing-cargo",
                    &["perf".into(), "bench".into(), "ci".into()],
                    &mut command
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn comparison_tool_uses_the_pin_and_is_owned_by_the_job() {
        let process = Process::recording(Path::new("/checkout"), Recording::default());
        let pins = fixture().pins;
        let tool = install(&process, &pins, "cargo").unwrap();
        let root = tool.path().to_path_buf();
        let recording = process.recorded().unwrap();
        assert_eq!(recording.steps().len(), 1);
        assert_eq!(
            &recording.steps()[0].args[..4],
            [
                "install",
                "--locked",
                "--version",
                pins.cargo_tool_version("critcmp").unwrap()
            ]
        );
        assert_eq!(recording.steps()[0].args.last().unwrap(), "critcmp");
        assert!(root.is_dir());
        drop(tool);
        assert!(!root.exists());
    }
}
