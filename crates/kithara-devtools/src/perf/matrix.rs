use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::{
    common::project::{PerfConfig, ProjectConfig},
    perf::{
        junit::parse_junit,
        lanes::{Lane, RepeatMeta, RunPaths, sanitize},
    },
    test::nextest_lane_command,
};

pub(crate) struct MatrixParams {
    pub(crate) data_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) target_root: PathBuf,
    pub(crate) repeats: u32,
    pub(crate) lanes: Vec<String>,
    pub(crate) extra: Vec<String>,
}

pub(crate) fn run(params: &MatrixParams, project: &ProjectConfig) -> Result<()> {
    let paths = RunPaths::new(&params.data_dir, &params.run_id);
    let lanes = resolve_lanes(&project.perf, &params.lanes)?;
    let commit = git_head()?;
    let matrix_dir = paths.matrix_dir();
    let slow_json = paths.slow_json();
    let slow_md = paths.slow_md();
    let report_md = paths.report_md();
    fs::create_dir_all(&matrix_dir).with_context(|| format!("create {}", matrix_dir.display()))?;
    println!(
        "[perf matrix] run={} matrix={} slow-json={} slow-md={} report={}",
        params.run_id,
        matrix_dir.display(),
        slow_json.display(),
        slow_md.display(),
        report_md.display()
    );
    let mut broken_lanes = Vec::new();
    'lanes: for lane in &lanes {
        let lane_name = lane.name();
        let target_dir = params.target_root.join(&lane_name);
        let profiles_dir = paths.profiles_dir(&sanitize(&lane_name));
        fs::create_dir_all(&profiles_dir)
            .with_context(|| format!("create {}", profiles_dir.display()))?;
        for repeat in 1..=params.repeats {
            let rep_dir = paths.repeat_dir(&lane_name, repeat);
            fs::create_dir_all(&rep_dir)
                .with_context(|| format!("create {}", rep_dir.display()))?;
            let mut extra = vec!["--profile".to_owned(), project.perf.nextest_profile.clone()];
            extra.extend(params.extra.iter().cloned());
            let (features, mut cmd) =
                nextest_lane_command(project, lane.flash, &lane.backend, &extra)?;
            cmd.env("CARGO_TARGET_DIR", &target_dir);
            let started_unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("clock before epoch")?
                .as_secs();
            let clock = Instant::now();
            println!("[perf matrix] {lane_name} rep{repeat}: running full suite");
            let status = cmd.status().context("spawn nextest")?;
            let duration_secs = clock.elapsed().as_secs_f64();
            let junit_src = Path::new("target")
                .join("nextest")
                .join(&project.perf.nextest_profile)
                .join("junit.xml");
            if !junit_src.exists() {
                eprintln!(
                    "[perf matrix] {lane_name}: no junit at {} - lane skipped (build failure?)",
                    junit_src.display()
                );
                broken_lanes.push(lane_name.clone());
                continue 'lanes;
            }
            fs::copy(&junit_src, rep_dir.join("junit.xml"))
                .with_context(|| format!("copy junit for {lane_name} rep{repeat}"))?;
            let junit_xml = fs::read_to_string(&junit_src)
                .with_context(|| format!("read junit for {lane_name} rep{repeat}"))?;
            let cases = parse_junit(&junit_xml)
                .with_context(|| format!("parse junit for {lane_name} rep{repeat}"))?;
            let failed_cases = cases.iter().filter(|case| case.failed).count();
            let test_secs = cases.iter().map(|case| case.secs).sum::<f64>();
            let first_failed = cases.iter().find(|case| case.failed).map_or_else(
                || "-".to_owned(),
                |case| format!("{}::{}", case.suite, case.name),
            );
            let meta = RepeatMeta {
                run_id: params.run_id.clone(),
                lane: lane_name.clone(),
                features: features.clone(),
                repeat,
                commit: commit.clone(),
                started_unix,
                duration_secs,
                exit_code: status.code(),
            };
            let meta_json = serde_json::to_vec_pretty(&meta).context("serialize meta")?;
            fs::write(rep_dir.join("meta.json"), meta_json)
                .with_context(|| format!("write meta for {lane_name} rep{repeat}"))?;
            println!(
                "[perf matrix] {lane_name} rep{repeat}: {duration_secs:.1}s exit={:?} cases={} failed={} test-secs={test_secs:.1} first-failed={first_failed}",
                status.code(),
                cases.len(),
                failed_cases
            );
        }
    }
    if !broken_lanes.is_empty() {
        bail!("lanes without junit (skipped): {}", broken_lanes.join(", "));
    }
    Ok(())
}

fn resolve_lanes(config: &PerfConfig, names: &[String]) -> Result<Vec<Lane>> {
    let configured = Lane::configured(config);
    if configured.is_empty() {
        bail!("perf.lanes is empty");
    }
    if names.is_empty() {
        return Ok(configured);
    }
    names
        .iter()
        .map(|name| {
            Lane::by_name(config, name).ok_or_else(|| {
                let valid = configured
                    .iter()
                    .map(Lane::name)
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::anyhow!("unknown lane `{name}`; valid: {valid}")
            })
        })
        .collect()
}

fn git_head() -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("git rev-parse HEAD")?;
    if !out.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
