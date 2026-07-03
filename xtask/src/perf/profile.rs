use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    common::project::ProjectConfig,
    perf::{
        lanes::{Lane, RunPaths, sanitize},
        list::nextest_list,
        slow::SlowReport,
    },
};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct IsolatedRun {
    pub(crate) suite: String,
    pub(crate) name: String,
    pub(crate) secs: f64,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) profile_path: PathBuf,
}

pub(crate) struct ProfileParams {
    pub(crate) data_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) target_root: PathBuf,
    pub(crate) lane: String,
    pub(crate) timeout_secs: u64,
    pub(crate) limit: Option<usize>,
}

pub(crate) fn samply_command(out: &Path, binary: &Path, test: &str) -> Command {
    let mut cmd = Command::new("samply");
    cmd.arg("record")
        .arg("--save-only")
        .arg("--output")
        .arg(out)
        .arg("--")
        .arg(binary)
        .arg(test)
        .arg("--exact")
        .arg("--nocapture");
    cmd
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<(Option<i32>, bool)> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("poll samply child")? {
            return Ok((status.code(), false));
        }
        if start.elapsed() >= timeout {
            child.kill().context("kill timed-out samply child")?;
            let _ = child.wait();
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn run(params: &ProfileParams) -> Result<()> {
    let paths = RunPaths::new(&params.data_dir, &params.run_id);
    let slow_json_path = paths.slow_json();
    let slow_json = fs::read_to_string(&slow_json_path)
        .with_context(|| format!("read {}", slow_json_path.display()))?;
    let slow: SlowReport = serde_json::from_str(&slow_json).context("parse slow.json")?;
    let Some(lane) = Lane::by_name(&params.lane) else {
        bail!("unknown lane `{}`", params.lane);
    };
    let project = ProjectConfig::load(Path::new("."))?;
    let target_dir = params.target_root.join(&params.lane);
    let suites = nextest_list(&project, lane.flash, lane.backend, &target_dir)?;
    let out_root = paths.profiles_dir(&params.lane);
    let mut isolated = Vec::new();
    let selected = slow.tests.iter().take(params.limit.unwrap_or(usize::MAX));
    for test in selected {
        let Some(suite) = suites.get(&test.suite) else {
            println!(
                "[perf profile] SKIP {} {} (suite not in list)",
                test.suite, test.name
            );
            continue;
        };
        let dir = out_root.join(sanitize(&test.suite));
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let out = dir.join(format!("{}.profile.json", sanitize(&test.name)));
        let mut cmd = samply_command(&out, &suite.binary_path, &test.name);
        cmd.current_dir(&suite.cwd);
        let clock = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!("samply not found on PATH - install with `cargo install samply`")
            }
            Err(err) => return Err(err).context("spawn samply"),
        };
        let (exit_code, timed_out) =
            wait_with_timeout(&mut child, Duration::from_secs(params.timeout_secs))?;
        let secs = clock.elapsed().as_secs_f64();
        println!(
            "[perf profile] {} {}: {secs:.1}s exit={exit_code:?} timed_out={timed_out}",
            test.suite, test.name
        );
        isolated.push(IsolatedRun {
            suite: test.suite.clone(),
            name: test.name.clone(),
            secs,
            exit_code,
            timed_out,
            profile_path: out,
        });
    }
    fs::create_dir_all(&out_root).with_context(|| format!("create {}", out_root.display()))?;
    let json = serde_json::to_vec_pretty(&isolated).context("serialize isolated runs")?;
    fs::write(out_root.join("isolated.json"), json).context("write isolated.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn samply_command_shape() {
        let cmd = samply_command(
            Path::new("/out/p.profile.json"),
            Path::new("/bin/suite_light-abc"),
            "offline::gapless",
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "samply");
        assert_eq!(
            args,
            [
                "record",
                "--save-only",
                "--output",
                "/out/p.profile.json",
                "--",
                "/bin/suite_light-abc",
                "offline::gapless",
                "--exact",
                "--nocapture"
            ]
        );
    }
}
