use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    path::Path,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::{Ctx, common::timestamp::utc_timestamp};
use tracing::info;

use super::process::Process;

pub(super) fn gate(ctx: &Ctx, arguments: &[OsString]) -> Result<()> {
    gate_with(&Process::new(&ctx.root, BTreeMap::new()), arguments)
}

fn gate_with(process: &Process, arguments: &[OsString]) -> Result<()> {
    let mut on = process.command("just");
    on.args(["test", "run", "--flash=on", "--no-block=on"])
        .args(arguments);
    process.run_command(&mut on, "gate: flash ON + no-block ON")?;
    let mut off = process.command("just");
    off.args(["test", "run", "--flash=off"])
        .args(arguments)
        .env(
            "CARGO_TARGET_DIR",
            process
                .environment_path("CARGO_TARGET_DIR")
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| process.target_dir())
                .join("flash-off"),
        );
    process.run_command(&mut off, "gate: flash OFF")
}

pub(super) fn full(ctx: &Ctx, output: &Path) -> Result<()> {
    let process = Process::new(&ctx.root, BTreeMap::new());
    let output = ctx.root.join(output);
    fs::create_dir_all(&output).context("creating CI cycle output")?;
    let run_id = utc_timestamp().replace(['-', ':'], "");
    fs::write(output.join("run-id"), format!("{run_id}\n"))?;
    let mutants = output.join("mutants");
    let mut failed = Vec::new();
    for (stage, args) in [
        ("lint", vec![OsString::from("lint"), "fast".into()]),
        ("test", vec![OsString::from("test")]),
        (
            "mutants",
            vec![
                OsString::from("test"),
                "mutants".into(),
                "run-all".into(),
                "--output".into(),
                mutants.into_os_string(),
            ],
        ),
    ] {
        let log = File::create(output.join(format!("stage-{stage}.log")))?;
        let mut command = process.command("just");
        command.args(args).stdout(log.try_clone()?).stderr(log);
        info!(stage, %run_id, "starting CI cycle stage");
        let started = Instant::now();
        let status = command
            .status()
            .with_context(|| format!("starting CI stage {stage}"))?;
        process.time(stage, started.elapsed(), status.success());
        fs::write(
            output.join(format!("stage-{stage}.exit")),
            format!("{}\n", status.code().unwrap_or(1)),
        )?;
        info!(stage, %status, "finished CI cycle stage");
        if !status.success() {
            failed.push(stage);
        }
    }
    ensure!(
        failed.is_empty(),
        "CI stages failed: {} (logs: {})",
        failed.join(", "),
        output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsString, path::Path};

    use super::{Process, gate_with};
    use crate::ci::process::Recording;

    #[test]
    fn gate_runs_both_modes_and_isolates_the_flash_off_target() {
        let process = Process::recording(Path::new("/repo"), Recording::default());
        gate_with(&process, &[OsString::from("--lane=workspace")]).unwrap();
        let recording = process.recorded().unwrap();
        let steps = recording.steps();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].args,
            [
                "test",
                "run",
                "--flash=on",
                "--no-block=on",
                "--lane=workspace"
            ]
        );
        assert_eq!(
            steps[1].args,
            ["test", "run", "--flash=off", "--lane=workspace"]
        );
        assert_eq!(steps[0].env, BTreeMap::new());
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .filter(|path| !path.is_empty())
            .map_or_else(|| Path::new("/repo/target").to_path_buf(), Into::into)
            .join("flash-off");
        assert_eq!(
            steps[1].env.get("CARGO_TARGET_DIR"),
            Some(&target.display().to_string())
        );
    }
}
