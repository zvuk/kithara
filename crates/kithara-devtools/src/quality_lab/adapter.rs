use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use super::manifest::Tool;

const CHA_PLUGINS: &str = "data_clumps,feature_envy,inappropriate_intimacy,shotgun_surgery,\
                           divergent_change,speculative_generality,async_callback_leak";
const CRAP_THRESHOLD: f64 = 30.0;

pub(super) struct AdapterOptions<'a> {
    pub(super) lcov: Option<&'a Path>,
    pub(super) baseline: Option<&'a Path>,
}

pub(super) struct ToolSpec {
    pub(super) program: String,
    pub(super) version_args: &'static [&'static str],
    pub(super) invocations: Vec<InvocationSpec>,
    pub(super) requires_clean_clone: bool,
}

pub(super) struct InvocationSpec {
    pub(super) name: &'static str,
    pub(super) args: Vec<String>,
    pub(super) artifact: &'static str,
    pub(super) finding_exit_codes: &'static [i32],
    pub(super) report: ReportKind,
}

#[derive(Clone, Copy)]
pub(super) enum ReportKind {
    CargoCrapDelta { threshold: f64 },
    Json,
    JsonStream,
    RustqualJson,
}

pub(super) fn build(
    tool: Tool,
    workspace_root: &Path,
    options: &AdapterOptions<'_>,
) -> Result<ToolSpec> {
    match tool {
        Tool::CargoCrap => cargo_crap(workspace_root, options),
        Tool::Cha => Ok(cha()),
        Tool::Rustqual => Ok(rustqual(workspace_root)),
        Tool::CargoDupes => Ok(cargo_dupes(workspace_root)),
        Tool::Pmat => Ok(pmat(workspace_root)),
    }
}

fn cargo_crap(_workspace_root: &Path, options: &AdapterOptions<'_>) -> Result<ToolSpec> {
    let lcov = required_file(options.lcov, "cargo-crap requires --lcov <FILE>")?;
    let mut args = vec![
        "--workspace".to_owned(),
        "--lcov".to_owned(),
        path_arg(&lcov),
        "--format".to_owned(),
        "json".to_owned(),
        "--sort".to_owned(),
        "file".to_owned(),
        "--missing".to_owned(),
        "pessimistic".to_owned(),
        "--threshold".to_owned(),
        CRAP_THRESHOLD.to_string(),
        "--exclude".to_owned(),
        "fuzz/**".to_owned(),
    ];
    let (finding_exit_codes, report): (&'static [i32], ReportKind) =
        if let Some(baseline) = options.baseline {
            let baseline = canonical_file(baseline, "cargo-crap baseline")?;
            args.push("--baseline".to_owned());
            args.push(path_arg(&baseline));
            args.push("--fail-regression".to_owned());
            (
                &[1],
                ReportKind::CargoCrapDelta {
                    threshold: CRAP_THRESHOLD,
                },
            )
        } else {
            (&[], ReportKind::Json)
        };
    Ok(ToolSpec {
        program: Tool::CargoCrap.program().to_owned(),
        version_args: &["--version"],
        invocations: vec![InvocationSpec {
            name: "coverage-risk",
            args,
            artifact: "report.json",
            finding_exit_codes,
            report,
        }],
        requires_clean_clone: false,
    })
}

fn cha() -> ToolSpec {
    ToolSpec {
        program: Tool::Cha.program().to_owned(),
        version_args: &["--version"],
        invocations: vec![
            InvocationSpec {
                name: "hotspot-100",
                args: strings(&[
                    "hotspot", "--count", "100", "--top", "50", "--format", "json",
                ]),
                artifact: "hotspot-100.json",
                finding_exit_codes: &[],
                report: ReportKind::Json,
            },
            InvocationSpec {
                name: "hotspot-500",
                args: strings(&[
                    "hotspot", "--count", "500", "--top", "50", "--format", "json",
                ]),
                artifact: "hotspot-500.json",
                finding_exit_codes: &[],
                report: ReportKind::Json,
            },
            InvocationSpec {
                name: "layers",
                args: strings(&["layers", "--format", "json"]),
                artifact: "layers.json",
                finding_exit_codes: &[],
                report: ReportKind::Json,
            },
            InvocationSpec {
                name: "smells",
                args: vec![
                    "analyze".to_owned(),
                    "--no-cache".to_owned(),
                    "--plugin".to_owned(),
                    CHA_PLUGINS.to_owned(),
                    "--format".to_owned(),
                    "json".to_owned(),
                    "--fail-on".to_owned(),
                    "warning".to_owned(),
                    "crates".to_owned(),
                    "xtask".to_owned(),
                ],
                artifact: "smells.json",
                finding_exit_codes: &[1],
                report: ReportKind::Json,
            },
        ],
        requires_clean_clone: true,
    }
}

fn rustqual(workspace_root: &Path) -> ToolSpec {
    let config = workspace_root.join(".config/quality-lab/rustqual.toml");
    ToolSpec {
        program: Tool::Rustqual.program().to_owned(),
        version_args: &["--version"],
        invocations: vec![InvocationSpec {
            name: "test-quality",
            args: vec![
                ".".to_owned(),
                "--config".to_owned(),
                path_arg(&config),
                "--format".to_owned(),
                "json".to_owned(),
                "--no-fail".to_owned(),
            ],
            artifact: "report.json",
            finding_exit_codes: &[],
            report: ReportKind::RustqualJson,
        }],
        requires_clean_clone: false,
    }
}

fn cargo_dupes(workspace_root: &Path) -> ToolSpec {
    ToolSpec {
        program: Tool::CargoDupes.program().to_owned(),
        version_args: &["--version"],
        invocations: vec![InvocationSpec {
            name: "sub-function-duplicates",
            args: vec![
                "--path".to_owned(),
                path_arg(workspace_root),
                "--exclude-tests".to_owned(),
                "--sub-function".to_owned(),
                "--min-lines".to_owned(),
                "8".to_owned(),
                "--min-sub-nodes".to_owned(),
                "12".to_owned(),
                "--threshold".to_owned(),
                "0.90".to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
                "report".to_owned(),
            ],
            artifact: "report.json-stream",
            finding_exit_codes: &[],
            report: ReportKind::JsonStream,
        }],
        requires_clean_clone: false,
    }
}

fn pmat(workspace_root: &Path) -> ToolSpec {
    ToolSpec {
        program: Tool::Pmat.program().to_owned(),
        version_args: &["--version"],
        invocations: vec![InvocationSpec {
            name: "repository-score",
            args: vec![
                "repo-score".to_owned(),
                "--path".to_owned(),
                path_arg(workspace_root),
                "--format".to_owned(),
                "json".to_owned(),
                "--deep".to_owned(),
            ],
            artifact: "report.json",
            finding_exit_codes: &[],
            report: ReportKind::Json,
        }],
        requires_clean_clone: false,
    }
}

fn required_file(path: Option<&Path>, message: &str) -> Result<PathBuf> {
    let Some(path) = path else {
        bail!("{message}");
    };
    canonical_file(path, "LCOV input")
}

fn canonical_file(path: &Path, label: &str) -> Result<PathBuf> {
    let path =
        fs::canonicalize(path).with_context(|| format!("resolve {label}: {}", path.display()))?;
    if !path.is_file() {
        bail!("{label} is not a file: {}", path.display());
    }
    Ok(path)
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn cargo_crap_requires_lcov() {
        let temp = tempdir().expect("tempdir");
        let error = build(
            Tool::CargoCrap,
            temp.path(),
            &AdapterOptions {
                lcov: None,
                baseline: None,
            },
        )
        .err()
        .expect("missing LCOV must fail");

        assert!(error.to_string().contains("requires --lcov"));
    }

    #[test]
    fn cargo_crap_only_enables_regression_exit_with_baseline() {
        let temp = tempdir().expect("tempdir");
        let lcov = temp.path().join("lcov.info");
        let baseline = temp.path().join("baseline.json");
        fs::write(&lcov, "TN:\n").expect("lcov");
        fs::write(&baseline, "{}").expect("baseline");

        let absolute = build(
            Tool::CargoCrap,
            temp.path(),
            &AdapterOptions {
                lcov: Some(&lcov),
                baseline: None,
            },
        )
        .expect("absolute spec");
        let delta = build(
            Tool::CargoCrap,
            temp.path(),
            &AdapterOptions {
                lcov: Some(&lcov),
                baseline: Some(&baseline),
            },
        )
        .expect("delta spec");

        assert!(absolute.invocations[0].finding_exit_codes.is_empty());
        assert_eq!(delta.invocations[0].finding_exit_codes, &[1]);
        assert!(
            delta.invocations[0]
                .args
                .contains(&"--fail-regression".to_owned())
        );
    }

    #[test]
    fn cargo_dupes_runs_only_the_sub_function_profile() {
        let temp = tempdir().expect("tempdir");
        let spec = build(
            Tool::CargoDupes,
            temp.path(),
            &AdapterOptions {
                lcov: None,
                baseline: None,
            },
        )
        .expect("cargo-dupes spec");

        let args = &spec.invocations[0].args;
        assert!(args.contains(&"--sub-function".to_owned()));
        assert!(args.contains(&"--exclude-tests".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("report"));
    }

    #[test]
    fn rustqual_exit_is_owned_by_test_quality_json_fields() {
        let temp = tempdir().expect("tempdir");
        let spec = build(
            Tool::Rustqual,
            temp.path(),
            &AdapterOptions {
                lcov: None,
                baseline: None,
            },
        )
        .expect("rustqual spec");

        assert!(spec.invocations[0].args.contains(&"--no-fail".to_owned()));
        assert!(spec.invocations[0].finding_exit_codes.is_empty());
    }

    #[test]
    fn pmat_adapter_is_read_only_and_machine_readable() {
        let temp = tempdir().expect("tempdir");
        let spec = build(
            Tool::Pmat,
            temp.path(),
            &AdapterOptions {
                lcov: None,
                baseline: None,
            },
        )
        .expect("PMAT spec");
        let args = &spec.invocations[0].args;

        assert_eq!(args.first().map(String::as_str), Some("repo-score"));
        assert!(args.contains(&"--path".to_owned()));
        assert!(args.contains(&"json".to_owned()));
        assert!(args.contains(&"--deep".to_owned()));
        assert!(!args.contains(&"--update-badge".to_owned()));
        assert!(!args.contains(&"--output".to_owned()));
    }
}
