use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    common::project::ProjectConfig,
    perf::{
        lanes::{Lane, RunPaths, sanitize},
        list::nextest_list,
    },
};

pub(crate) struct TraceParams {
    pub(crate) data_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) target_root: PathBuf,
    pub(crate) lane: String,
    pub(crate) suite: String,
    pub(crate) test: String,
}

pub(crate) fn xctrace_command(out: &Path, binary: &Path, test: &str) -> Command {
    let mut cmd = Command::new("xcrun");
    cmd.arg("xctrace")
        .arg("record")
        .arg("--template")
        .arg("Time Profiler")
        .arg("--output")
        .arg(out)
        .arg("--launch")
        .arg("--")
        .arg(binary)
        .arg(test)
        .arg("--exact")
        .arg("--nocapture");
    cmd
}

pub(crate) fn run(params: &TraceParams, project: &ProjectConfig) -> Result<()> {
    let Some(lane) = Lane::by_name(&project.perf, &params.lane) else {
        bail!("unknown lane `{}`", params.lane);
    };
    let target_dir = params.target_root.join(&params.lane);
    let suites = nextest_list(project, lane.flash, &lane.backend, &target_dir)?;
    let Some(suite) = suites.get(&params.suite) else {
        bail!("suite `{}` not found in nextest list", params.suite);
    };
    let paths = RunPaths::new(&params.data_dir, &params.run_id);
    let out_dir = paths.run_dir.join("traces");
    fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let out = out_dir.join(format!("{}.trace", sanitize(&params.test)));
    let mut cmd = xctrace_command(&out, &suite.binary_path, &params.test);
    cmd.current_dir(&suite.cwd);
    let status = cmd.status().context("run xctrace")?;
    if !status.success() {
        bail!("xctrace failed with {:?}", status.code());
    }
    println!("[perf trace] open {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xctrace_command_shape() {
        let cmd = xctrace_command(
            Path::new("/out/t.trace"),
            Path::new("/bin/suite_light-abc"),
            "offline::gapless",
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "xcrun");
        assert_eq!(
            args,
            [
                "xctrace",
                "record",
                "--template",
                "Time Profiler",
                "--output",
                "/out/t.trace",
                "--launch",
                "--",
                "/bin/suite_light-abc",
                "offline::gapless",
                "--exact",
                "--nocapture"
            ]
        );
    }
}
