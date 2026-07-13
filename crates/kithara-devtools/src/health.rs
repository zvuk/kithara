use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::Args;

use crate::{
    common::{project::ProjectConfig, timestamp::utc_timestamp},
    stages::{SharedStage, StageCommand},
};

struct Consts;
impl Consts {
    /// Substrings that mark an environment-level failure rather than a real
    /// regression — typically a missing tool or unpublished baseline.
    /// When any of these appear in the stage log on non-zero exit the stage
    /// is reported as SKIP instead of FAIL.
    const ENV_SKIP_MARKERS: &'static [&'static str] = &[
        "no such command:",
        "command not found",
        "not found in registry",
        "Library not loaded",
    ];
    const LOGS_DIR: &'static str = "target/health-logs";
    const REPORT_PATH: &'static str = "target/health-report.md";
    const STDOUT_TAIL_LINES: usize = 80;
}

#[derive(Debug, Args)]
pub struct HealthArgs {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

struct Stage {
    name: &'static str,
    program: &'static str,
    args: Vec<String>,
    advisory: bool,
}

impl Stage {
    fn new(name: &'static str, program: &'static str, args: &[&str]) -> Self {
        Self {
            name,
            program,
            args: args.iter().map(|s| (*s).to_string()).collect(),
            advisory: false,
        }
    }

    fn advisory(mut self) -> Self {
        self.advisory = true;
        self
    }

    fn exclude_crates(mut self, crates: &[String]) -> Self {
        for krate in crates {
            self.args.push("--exclude".to_owned());
            self.args.push(krate.clone());
        }
        self
    }

    fn shared(name: &'static str, stage: SharedStage) -> Self {
        let StageCommand { program, args } = stage.health_command();
        Self {
            name,
            program,
            args,
            advisory: false,
        }
    }
}

struct StageResult {
    name: &'static str,
    duration: Duration,
    note: Option<String>,
    status: Status,
    cmdline: String,
}

pub(crate) fn run(_args: &HealthArgs) -> Result<()> {
    let project = ProjectConfig::load(Path::new("."))?;
    let logs_dir = PathBuf::from(Consts::LOGS_DIR);
    fs::create_dir_all(&logs_dir).context("create health-logs directory")?;

    let stages = build_stages(&project);
    let total_start = Instant::now();
    let mut results = Vec::with_capacity(stages.len());

    for (idx, stage) in stages.iter().enumerate() {
        let result = run_stage(idx + 1, stage, &logs_dir);
        print_progress(idx + 1, stages.len(), &result);
        results.push(result);
    }

    let total = total_start.elapsed();
    write_report(&results, total, &logs_dir, &project.project.name)?;

    let failed = results.iter().filter(|r| r.status == Status::Fail).count();
    println!();
    println!(
        "health: {} stage(s) — {} failed in {}",
        results.len(),
        failed,
        format_duration(total),
    );
    println!("report: {}", Consts::REPORT_PATH);
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn build_stages(project: &ProjectConfig) -> Vec<Stage> {
    build_stages_with_excludes(
        &project.health.feature_powerset_exclude,
        &project.health.workspace_exclude,
    )
}

fn build_stages_with_excludes(
    feature_powerset_exclude: &[String],
    workspace_exclude: &[String],
) -> Vec<Stage> {
    vec![
        Stage::shared("format-check", SharedStage::FmtCheck),
        Stage::new(
            "markdown-format-check",
            "cargo",
            &["xtask", "format", "--check", "--only", "markdown"],
        )
        .advisory(),
        Stage::shared("clippy", SharedStage::Clippy),
        Stage::shared("ast-grep-advisory", SharedStage::AstGrep).advisory(),
        Stage::shared("xtask-lint", SharedStage::Lint),
        Stage::new("quality-report", "cargo", &["xtask", "quality", "report"]),
        Stage::shared("typos", SharedStage::Typos),
        Stage::shared("similarity-strict", SharedStage::Similarity).advisory(),
        Stage::shared("orphans", SharedStage::Orphans),
        Stage::new("machete", "cargo", &["machete"]),
        Stage::new("shear", "cargo", &["shear", "--deny-warnings"]),
        Stage::new("deny", "cargo", &["deny", "check"]),
        // NOTE: deliberately *not* passing `--no-dev-deps`. That flag asks
        Stage::new(
            "hack-feature-powerset",
            "cargo",
            &[
                "hack",
                "check",
                "--feature-powerset",
                "--depth",
                "2",
                "--workspace",
            ],
        )
        .exclude_crates(feature_powerset_exclude),
        Stage::new(
            "semver-checks",
            "cargo",
            &["semver-checks", "check-release", "--workspace"],
        )
        .exclude_crates(workspace_exclude),
        Stage::new(
            "geiger",
            "cargo",
            &[
                "geiger",
                "--all-targets",
                "--all-dependencies",
                "--output-format",
                "Ascii",
            ],
        )
        .advisory(),
        Stage::new(
            "lockbud-deadlock",
            "cargo",
            &["lockbud", "-k", "deadlock", "--workspace"],
        ),
        Stage::new("workspace-unused-pub", "cargo", &["workspace-unused-pub"]),
        Stage::new(
            "workspace-tests",
            "cargo",
            &["xtask", "test", "--lane=workspace"],
        ),
        Stage::new("doc-tests", "cargo", &["xtask", "test", "--lane=doc"]),
    ]
}

fn run_stage(idx: usize, stage: &Stage, logs_dir: &Path) -> StageResult {
    let cmdline = format!("{} {}", stage.program, stage.args.join(" "));
    let log_path = logs_dir.join(format!("{idx:02}-{}.log", stage.name));
    let log_file = match fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => {
            return StageResult {
                cmdline,
                name: stage.name,
                status: Status::Fail,
                note: Some(format!("failed to open log: {e}")),
                duration: Duration::ZERO,
            };
        }
    };
    let stderr_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return StageResult {
                cmdline,
                name: stage.name,
                status: Status::Fail,
                note: Some(format!("failed to clone log handle: {e}")),
                duration: Duration::ZERO,
            };
        }
    };
    let start = Instant::now();
    let status_result = Command::new(stage.program)
        .args(&stage.args)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file))
        .status();
    let duration = start.elapsed();

    match status_result {
        Ok(s) if s.success() => StageResult {
            cmdline,
            duration,
            name: stage.name,
            status: Status::Pass,
            note: None,
        },
        Ok(s) => {
            let exit = s.code().unwrap_or(-1);
            let (status, note) = match scan_env_skip_marker(&log_path) {
                Some(marker) => (Status::Skip, format!("environment: {marker} (exit {exit})")),
                None if stage.advisory => (Status::Warn, format!("exit {exit}")),
                None => (Status::Fail, format!("exit {exit}")),
            };
            StageResult {
                cmdline,
                status,
                duration,
                name: stage.name,
                note: Some(note),
            }
        }
        Err(e) => {
            let kind = e.kind();
            let is_missing = matches!(kind, std::io::ErrorKind::NotFound);
            StageResult {
                cmdline,
                duration,
                name: stage.name,
                status: if is_missing {
                    Status::Skip
                } else {
                    Status::Fail
                },
                note: Some(format!("{kind}: {e}")),
            }
        }
    }
}

fn print_progress(idx: usize, total: usize, r: &StageResult) {
    println!(
        "[{idx:02}/{total:02}] {:<22} {:<5} {}{}",
        r.name,
        r.status.label(),
        format_duration(r.duration),
        r.note
            .as_ref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default(),
    );
}

fn write_report(
    results: &[StageResult],
    total: Duration,
    logs_dir: &Path,
    project_name: &str,
) -> Result<()> {
    let mut out = String::new();
    let timestamp = utc_timestamp();
    let total_str = format_duration(total);
    let failed = results.iter().filter(|r| r.status == Status::Fail).count();
    let overall = if failed == 0 { "PASS" } else { "FAIL" };

    let title = if project_name.is_empty() {
        "health report".to_owned()
    } else {
        format!("{project_name} health report")
    };
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- generated_at_utc: {timestamp}\n"));
    out.push_str(&format!("- total_duration: {total_str}\n"));
    out.push_str(&format!(
        "- overall: {overall} ({} stage(s), {failed} failed)\n",
        results.len()
    ));
    out.push_str(&format!("- per-stage logs: `{}/`\n\n", logs_dir.display()));
    out.push_str("Excluded by design (run separately): `mutants`, `coverage`, `dead`, ");
    out.push_str(
        "`test --lane=e2e`, `test --lane=selenium`, `wasm`, `bench`, `perf`, `memory-check`.\n\n",
    );

    out.push_str("## Summary\n\n");
    out.push_str("| # | Stage | Status | Duration | Notes |\n");
    out.push_str("|---|-------|--------|----------|-------|\n");
    for (idx, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            idx + 1,
            r.name,
            r.status.label(),
            format_duration(r.duration),
            r.note.clone().unwrap_or_default(),
        ));
    }
    out.push('\n');

    out.push_str("## Stage details\n\n");
    for (idx, r) in results.iter().enumerate() {
        let log_path = logs_dir.join(format!("{:02}-{}.log", idx + 1, r.name));
        out.push_str(&format!(
            "### {}. {} — {} ({})\n\n",
            idx + 1,
            r.name,
            r.status.label(),
            format_duration(r.duration),
        ));
        out.push_str(&format!("```\n{}\n```\n\n", r.cmdline));
        if let Some(note) = &r.note {
            out.push_str(&format!("note: {note}\n\n"));
        }
        let tail = read_log_tail(&log_path, Consts::STDOUT_TAIL_LINES);
        if !tail.is_empty() {
            out.push_str(&format!(
                "<details><summary>last {} log lines (full: `{}`)</summary>\n\n```\n{}\n```\n\n</details>\n\n",
                Consts::STDOUT_TAIL_LINES,
                log_path.display(),
                tail,
            ));
        }
    }

    fs::write(Consts::REPORT_PATH, out).context("write health report")?;
    Ok(())
}

fn scan_env_skip_marker(path: &Path) -> Option<&'static str> {
    let content = fs::read_to_string(path).ok()?;
    Consts::ENV_SKIP_MARKERS
        .iter()
        .copied()
        .find(|m| content.contains(m))
}

fn read_log_tail(path: &Path, n: usize) -> String {
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}.{:01}s", d.subsec_millis() / 100)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use clap::Subcommand;

    use super::*;
    use crate::CoreCommand;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn write_log(content: &str) -> PathBuf {
        let idx = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kithara-xtask-health-{}-{idx}.log",
            std::process::id(),
        ));
        fs::write(&path, content).expect("write tmp log");
        path
    }

    #[test]
    fn health_xtask_stage_argv_is_parseable() {
        let stages = build_stages_with_excludes(&[], &[]);
        let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));

        for stage in stages
            .iter()
            .filter(|stage| stage.args.first().is_some_and(|arg| arg == "xtask"))
        {
            let argv =
                std::iter::once("xtask").chain(stage.args.iter().skip(1).map(String::as_str));
            command
                .clone()
                .try_get_matches_from(argv)
                .unwrap_or_else(|error| panic!("invalid health stage '{}': {error}", stage.name));
        }

        let scopes = [
            crate::common::scope::Scope::default(),
            crate::common::scope::Scope::new(vec!["kithara-bufpool".to_owned()], vec![]),
            crate::common::scope::Scope::new(vec![], vec!["tests".into()]),
        ];
        for scope in scopes {
            for stage in SharedStage::AUDIT {
                let stage_command = stage.audit_command(&scope);
                if stage_command.program != "xtask"
                    || stage_command
                        .args
                        .last()
                        .is_some_and(|arg| arg == "__skip__")
                {
                    continue;
                }
                let argv =
                    std::iter::once("xtask").chain(stage_command.args.iter().map(String::as_str));
                command
                    .clone()
                    .try_get_matches_from(argv)
                    .unwrap_or_else(|error| {
                        panic!("invalid audit stage '{}': {error}", stage.audit_name())
                    });
            }
        }
    }

    #[test]
    fn skip_marker_no_such_command() {
        let log = write_log("error: no such command: `hack`\n");
        assert_eq!(scan_env_skip_marker(&log), Some("no such command:"));
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn skip_marker_unpublished_baseline() {
        let log = write_log(
            "error: failed to retrieve index of crate versions from registry\n\
             Caused by:\n    kithara-abr not found in registry (crates.io).\n",
        );
        assert_eq!(scan_env_skip_marker(&log), Some("not found in registry"));
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn skip_marker_lockbud_dylib_drift() {
        let log = write_log("dyld[691]: Library not loaded: @rpath/librustc_driver-XXX.dylib\n");
        assert_eq!(scan_env_skip_marker(&log), Some("Library not loaded"));
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn skip_marker_genuine_failure_returns_none() {
        let log = write_log("test result: FAILED. 0 passed; 1 failed; 0 ignored\n");
        assert!(scan_env_skip_marker(&log).is_none());
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn skip_marker_missing_log_returns_none() {
        let path = Path::new("/nonexistent/health-log.txt");
        assert!(scan_env_skip_marker(path).is_none());
    }
}
