use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;

use super::{
    adapter::{self, AdapterOptions, ToolSpec},
    config::QualityLabConfig,
    manifest::{InvocationManifest, Profile, Status, Tool, ToolManifest},
    native, workspace,
};
use crate::common::process::{ProcessError, ProcessRequest, run_process};

const MANIFEST_SCHEMA_VERSION: u32 = 1;

pub(super) struct ToolRunContext<'a> {
    pub(super) options: &'a AdapterOptions<'a>,
    pub(super) revision_dir: &'a Path,
    pub(super) workspace_root: &'a Path,
    pub(super) config: &'a QualityLabConfig,
    pub(super) revision: &'a str,
    pub(super) profile: Profile,
}

pub(super) fn run_tool(
    context: &ToolRunContext<'_>,
    tool: Tool,
    profile_remaining: Duration,
) -> Result<ToolManifest> {
    let tool_config = context.config.tools.get(tool);
    let tool_dir = context.revision_dir.join(tool.to_string());
    workspace::reset_directory(&tool_dir)?;
    let started = Instant::now();
    let budget = Duration::from_secs(tool_config.timeout_secs).min(profile_remaining);
    let spec = match adapter::build(tool, context.workspace_root, context.options) {
        Ok(spec) => spec,
        Err(error) => {
            return finish_manifest(
                &tool_dir,
                ToolManifest {
                    tool,
                    schema_version: MANIFEST_SCHEMA_VERSION,
                    revision: context.revision.to_owned(),
                    profile: context.profile,
                    tool_version: tool_config.version.clone(),
                    status: Status::ToolError,
                    duration_ms: duration_ms(started.elapsed()),
                    invocations: Vec::new(),
                    note: Some(error.to_string()),
                },
            );
        }
    };

    let mut invocations = Vec::new();
    let mut run_root = context.workspace_root.to_path_buf();
    if spec.requires_clean_clone
        && let Err(error) = workspace::prepare_clean_clone(
            context.workspace_root,
            &tool_dir,
            context.revision,
            started,
            budget,
            &mut run_root,
        )
    {
        let note = cleanup_result(
            workspace::remove_scratch_if_present(&tool_dir.join("worktree")),
            error.to_string(),
        );
        return finish_manifest(
            &tool_dir,
            ToolManifest {
                tool,
                invocations,
                schema_version: MANIFEST_SCHEMA_VERSION,
                revision: context.revision.to_owned(),
                profile: context.profile,
                tool_version: tool_config.version.clone(),
                status: Status::ToolError,
                duration_ms: duration_ms(started.elapsed()),
                note: Some(note),
            },
        );
    }

    let (version_status, version_note) = check_version(
        &spec,
        &tool_dir,
        &run_root,
        &tool_config.version,
        started,
        budget,
        &mut invocations,
    );
    if let Some(status) = version_status {
        let status = if status == Status::Skipped && context.profile != Profile::Manual {
            Status::ToolError
        } else {
            status
        };
        let (status, note) = with_cleanup(&spec, &run_root, status, version_note);
        return finish_manifest(
            &tool_dir,
            ToolManifest {
                tool,
                status,
                invocations,
                note,
                schema_version: MANIFEST_SCHEMA_VERSION,
                revision: context.revision.to_owned(),
                profile: context.profile,
                tool_version: tool_config.version.clone(),
                duration_ms: duration_ms(started.elapsed()),
            },
        );
    }

    let (status, note) = run_invocations(
        &spec,
        &tool_dir,
        &run_root,
        started,
        budget,
        &mut invocations,
    );
    let (status, note) = with_cleanup(&spec, &run_root, status, note);
    finish_manifest(
        &tool_dir,
        ToolManifest {
            tool,
            status,
            invocations,
            note,
            schema_version: MANIFEST_SCHEMA_VERSION,
            revision: context.revision.to_owned(),
            profile: context.profile,
            tool_version: tool_config.version.clone(),
            duration_ms: duration_ms(started.elapsed()),
        },
    )
}

fn check_version(
    spec: &ToolSpec,
    tool_dir: &Path,
    run_root: &Path,
    expected: &str,
    started: Instant,
    budget: Duration,
    invocations: &mut Vec<InvocationManifest>,
) -> (Option<Status>, Option<String>) {
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return (
            Some(Status::ToolError),
            Some("tool time budget exhausted before version check".to_owned()),
        );
    }
    let args = spec
        .version_args
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect::<Vec<_>>();
    let stdout = tool_dir.join("version.stdout.log");
    let stderr = tool_dir.join("version.stderr.log");
    let outcome = match run_process(&ProcessRequest {
        program: &spec.program,
        args: &args,
        cwd: run_root,
        stdout_path: &stdout,
        stderr_path: &stderr,
        timeout: remaining,
    }) {
        Ok(outcome) => outcome,
        Err(ProcessError::MissingExecutable(_)) => {
            return (
                Some(Status::Skipped),
                Some(format!(
                    "optional Quality Lab executable `{}` is not installed",
                    spec.program
                )),
            );
        }
        Err(error) => {
            return (
                Some(Status::ToolError),
                Some(format!("run {} version check: {error}", spec.program)),
            );
        }
    };
    invocations.push(InvocationManifest {
        artifact: "version.stdout.log".to_owned(),
        command: command(&spec.program, &args),
        duration_ms: duration_ms(outcome.duration),
        exit_code: outcome.status.code(),
    });
    if outcome.timed_out {
        return (
            Some(Status::ToolError),
            Some(format!("{} version check timed out", spec.program)),
        );
    }
    if !outcome.status.success() {
        return (
            Some(Status::ToolError),
            Some(format!(
                "{} version check exited with {:?}",
                spec.program,
                outcome.status.code()
            )),
        );
    }
    let stdout_text = match fs::read_to_string(&stdout) {
        Ok(text) => text,
        Err(error) => {
            return (
                Some(Status::ToolError),
                Some(format!("read {} version output: {error}", spec.program)),
            );
        }
    };
    let stderr_text = match fs::read_to_string(&stderr) {
        Ok(text) => text,
        Err(error) => {
            return (
                Some(Status::ToolError),
                Some(format!(
                    "read {} version diagnostics: {error}",
                    spec.program
                )),
            );
        }
    };
    let output = format!("{stdout_text}\n{stderr_text}");
    if !contains_exact_version(&output, expected) {
        return (
            Some(Status::ToolError),
            Some(format!(
                "{} version mismatch: expected exact version {expected}, got `{}`",
                spec.program,
                output.trim()
            )),
        );
    }
    (None, None)
}

fn run_invocations(
    spec: &ToolSpec,
    tool_dir: &Path,
    run_root: &Path,
    started: Instant,
    budget: Duration,
    invocations: &mut Vec<InvocationManifest>,
) -> (Status, Option<String>) {
    let mut status = Status::Clean;
    for invocation in &spec.invocations {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return (
                Status::ToolError,
                Some(format!(
                    "tool time budget exhausted before `{}`",
                    invocation.name
                )),
            );
        }
        let artifact = tool_dir.join(invocation.artifact);
        let stderr_name = format!("{}.stderr.log", invocation.name);
        let outcome = match run_process(&ProcessRequest {
            program: &spec.program,
            args: &invocation.args,
            cwd: run_root,
            stdout_path: &artifact,
            stderr_path: &tool_dir.join(stderr_name),
            timeout: remaining,
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                return (
                    Status::ToolError,
                    Some(format!(
                        "run {} `{}`: {error}",
                        spec.program, invocation.name
                    )),
                );
            }
        };
        invocations.push(InvocationManifest {
            artifact: invocation.artifact.to_owned(),
            command: command(&spec.program, &invocation.args),
            duration_ms: duration_ms(outcome.duration),
            exit_code: outcome.status.code(),
        });
        if outcome.timed_out {
            return (
                Status::ToolError,
                Some(format!("{} `{}` timed out", spec.program, invocation.name)),
            );
        }
        let Some(exit_code) = outcome.status.code() else {
            return (
                Status::ToolError,
                Some(format!(
                    "{} `{}` terminated without an exit code",
                    spec.program, invocation.name
                )),
            );
        };
        let exit_has_findings = invocation.finding_exit_codes.contains(&exit_code);
        if exit_code != 0 && !exit_has_findings {
            return (
                Status::ToolError,
                Some(format!(
                    "{} `{}` exited with unexpected code {exit_code}",
                    spec.program, invocation.name
                )),
            );
        }
        match native::validate_report(&artifact, invocation) {
            Ok(report_has_findings) => {
                if exit_has_findings || report_has_findings {
                    status = Status::Findings;
                }
            }
            Err(error) => {
                return (
                    Status::ToolError,
                    Some(format!(
                        "validate {} `{}` report: {error}",
                        spec.program, invocation.name
                    )),
                );
            }
        }
    }
    (status, None)
}

fn contains_exact_version(output: &str, expected: &str) -> bool {
    output.split_whitespace().any(|token| {
        token.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '+' | '-' | '_')
        }) == expected
    })
}

fn with_cleanup(
    spec: &ToolSpec,
    run_root: &Path,
    status: Status,
    note: Option<String>,
) -> (Status, Option<String>) {
    if !spec.requires_clean_clone {
        return (status, note);
    }
    match workspace::remove_scratch_if_present(run_root) {
        Ok(()) => (status, note),
        Err(error) => {
            let note = note.map_or_else(
                || error.to_string(),
                |note| format!("{note}; scratch cleanup failed: {error}"),
            );
            (Status::ToolError, Some(note))
        }
    }
}

fn cleanup_result(result: Result<()>, note: String) -> String {
    match result {
        Ok(()) => note,
        Err(error) => format!("{note}; scratch cleanup failed: {error}"),
    }
}

pub(super) fn finish_manifest(tool_dir: &Path, manifest: ToolManifest) -> Result<ToolManifest> {
    manifest.write(&tool_dir.join("manifest.json"))?;
    Ok(manifest)
}

pub(super) fn error_manifest(
    revision: &str,
    profile: Profile,
    tool: Tool,
    version: &str,
    note: &str,
) -> ToolManifest {
    ToolManifest {
        profile,
        tool,
        schema_version: MANIFEST_SCHEMA_VERSION,
        revision: revision.to_owned(),
        tool_version: version.to_owned(),
        status: Status::ToolError,
        duration_ms: 0,
        invocations: Vec::new(),
        note: Some(note.to_owned()),
    }
}

fn command(program: &str, args: &[String]) -> Vec<String> {
    std::iter::once(program.to_owned())
        .chain(args.iter().cloned())
        .collect()
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::quality_lab::adapter::{InvocationSpec, ReportKind};

    /// Budget for the tests whose subject is not the budget.
    ///
    /// These prove how an invocation's exit status and output become a
    /// [`Status`]. A reachable budget can only change that by killing the
    /// child, which reports no exit code and lands in the tool-error arm — so
    /// on a busy host the assertion would be answering a question about the
    /// machine. The budget-exhausted arms are proven by their own doubles.
    const NOT_UNDER_TEST: Duration = Duration::MAX;

    /// The name of the double's binary target.
    const DOUBLE: &str = "kithara-devtools-fake-tool";

    /// The version the double answers `--version` with.
    ///
    /// It reaches the double through its behaviour file rather than being
    /// compiled into it, so this test and the double cannot drift apart.
    const DOUBLE_VERSION: &str = "1.2.3";

    #[test]
    fn version_matching_rejects_prefix_versions() {
        assert!(contains_exact_version("rustqual 1.8.1", "1.8.1"));
        assert!(!contains_exact_version("rustqual 1.8.10", "1.8.1"));
        assert!(!contains_exact_version("rustqual v1.8.1", "1.8.1"));
    }

    #[test]
    fn maps_expected_nonzero_exit_to_findings_after_version_check() {
        let temp = tempfile::tempdir().expect("tempdir");
        let spec = fake_spec(&temp, r#"{"findings":1}"#, 1);
        let mut invocations = Vec::new();
        let started = Instant::now();
        let budget = NOT_UNDER_TEST;

        let (version_status, version_note) = check_version(
            &spec,
            temp.path(),
            temp.path(),
            DOUBLE_VERSION,
            started,
            budget,
            &mut invocations,
        );
        let (status, note) = run_invocations(
            &spec,
            temp.path(),
            temp.path(),
            started,
            budget,
            &mut invocations,
        );

        assert_eq!(version_status, None);
        assert_eq!(version_note, None);
        assert_eq!(status, Status::Findings);
        assert_eq!(note, None);
        assert_eq!(invocations.len(), 2);
    }

    #[test]
    fn malformed_native_output_is_a_tool_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let spec = fake_spec(&temp, "{", 0);
        let mut invocations = Vec::new();

        let (status, note) = run_invocations(
            &spec,
            temp.path(),
            temp.path(),
            Instant::now(),
            NOT_UNDER_TEST,
            &mut invocations,
        );

        assert_eq!(status, Status::ToolError);
        assert!(
            note.as_deref()
                .is_some_and(|note| note.contains("parse JSON report"))
        );
    }

    /// The double's file name, which keeps the platform's executable suffix.
    ///
    /// Windows refuses to start a file that has lost its `.exe`, so the copy
    /// each test makes is named the same way as the binary Cargo built.
    fn double_name() -> String {
        format!("{DOUBLE}{}", env::consts::EXE_SUFFIX)
    }

    /// Where Cargo put the double, which a unit test cannot ask it for.
    ///
    /// `CARGO_BIN_EXE_` is an integration-test macro, and reaching it would
    /// mean publishing this module. The test binary sits one directory below
    /// where Cargo writes binary targets.
    fn double() -> PathBuf {
        let harness = env::current_exe().expect("current test executable");
        let built = harness
            .parent()
            .and_then(Path::parent)
            .expect("the test binary sits under a target directory")
            .join(double_name());
        assert!(
            built.is_file(),
            "{} is not built; run `cargo build -p kithara-devtools --bin {DOUBLE}` first",
            built.display()
        );
        built
    }

    fn fake_spec(temp: &TempDir, report: &str, exit_code: i32) -> ToolSpec {
        let program = temp.path().join(double_name());
        fs::copy(double(), &program).expect("copy the double");
        fs::write(
            program.with_extension("behaviour"),
            format!("{DOUBLE} {DOUBLE_VERSION}\n{exit_code}\n{report}\n"),
        )
        .expect("behaviour");
        ToolSpec {
            program: program.to_string_lossy().into_owned(),
            version_args: &["--version"],
            invocations: vec![InvocationSpec {
                name: "analyze",
                args: vec!["analyze".to_owned()],
                artifact: "report.json",
                finding_exit_codes: &[1],
                report: ReportKind::Json,
            }],
            requires_clean_clone: false,
        }
    }
}
