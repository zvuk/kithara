use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml::Table;

const CONFIG_REL: &str = ".config/xtask.toml";

/// Project-specific identity and per-tool settings for the otherwise
/// project-agnostic xtask. Loaded from `.config/xtask.toml`; every field
/// defaults to empty so a fresh project starts with no baked-in names and
/// fills in only what it uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub architecture: ArchitectureConfig,
    pub audit_clippy: AuditClippyConfig,
    pub health: HealthConfig,
    pub lint_exclude: LintExcludeConfig,
    pub orphans: OrphansConfig,
    pub perf: PerfConfig,
    pub project: ProjectIdentity,
    pub quality: QualityConfig,
    #[serde(default)]
    pub ext: Table,
    pub test: TestCommandConfig,
    #[serde(default, rename = "workspace-scan")]
    pub workspace_scan: WorkspaceScan,
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureConfig {
    pub filters: ArchitectureFilterConfig,
    pub runtime: ArchitectureRuntimeConfig,
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureFilterConfig {
    pub exclude_crates: Vec<String>,
    pub exclude_modules: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureRuntimeConfig {
    pub scenarios: Vec<RuntimeScenarioConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RuntimeScenarioConfig {
    Test {
        name: String,
        package: String,
        test: String,
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        ignored: bool,
        timeout_secs: u64,
    },
    Binary {
        name: String,
        package: String,
        bin: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        features: Vec<String>,
        timeout_secs: u64,
    },
    Trace {
        name: String,
        path: String,
    },
}

impl RuntimeScenarioConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Test { name, .. } | Self::Binary { name, .. } | Self::Trace { name, .. } => name,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.name().is_empty()
            || !self.name().chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            bail!(
                "architecture runtime scenario name `{}` must use only ASCII letters, digits, '-' or '_'",
                self.name()
            );
        }
        match self {
            Self::Test {
                package,
                test,
                timeout_secs,
                ..
            } => {
                if package.is_empty() || test.is_empty() {
                    bail!(
                        "architecture runtime test scenario `{}` requires package and test",
                        self.name()
                    );
                }
                if *timeout_secs == 0 {
                    bail!(
                        "architecture runtime scenario `{}` timeout_secs must be positive",
                        self.name()
                    );
                }
            }
            Self::Binary {
                package,
                bin,
                timeout_secs,
                ..
            } => {
                if package.is_empty() || bin.is_empty() {
                    bail!(
                        "architecture runtime binary scenario `{}` requires package and bin",
                        self.name()
                    );
                }
                if *timeout_secs == 0 {
                    bail!(
                        "architecture runtime scenario `{}` timeout_secs must be positive",
                        self.name()
                    );
                }
            }
            Self::Trace { path, .. } if path.is_empty() => {
                bail!(
                    "architecture runtime trace scenario `{}` requires path",
                    self.name()
                );
            }
            Self::Trace { .. } => {}
        }
        Ok(())
    }
}

/// Extended advisory clippy lints for the opt-in `just lint audit-clippy` sweep.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuditClippyConfig {
    pub lints: Vec<String>,
}

/// Workspace-wide Rust file scan exclusions.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceScan {
    pub exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrphansConfig {
    /// Packages excluded from the default `cargo modules orphans` sweep
    /// (generated/helper/macro crates and per-target-gated crates that the
    /// default rust-analyzer view flags as false-positive orphans).
    pub exclude_packages: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityConfig {
    /// Repository-specific heavyweight stages used by `quality assess --depth deep`.
    pub assessment: QualityAssessmentConfig,
    /// Trait directory whose every `pub trait` must carry workspace mock coverage.
    pub unimock_traits_dir: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentConfig {
    pub deep_stages: Vec<QualityAssessmentStageConfig>,
    pub not_applicable_tools: Vec<QualityAssessmentToolPolicyConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentStageConfig {
    pub name: String,
    pub command: Vec<String>,
    pub expected_artifacts: Vec<String>,
    pub platforms: Vec<String>,
    pub tools: Vec<String>,
    pub complete_only: bool,
    pub hard_invariant: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentToolPolicyConfig {
    pub reason: String,
    pub tool: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintExcludeConfig {
    /// Inline-module names / `::`-paths whose violations are dropped from every
    /// lint namespace, regardless of file.
    pub modules: Vec<String>,
    /// Workspace-relative globs whose violations are dropped from every lint
    /// namespace (`arch`, `style`, `idioms`) so baselines measure production
    /// debt, not test code. `#[cfg(test)]` blocks are stripped automatically
    /// (AST) on top of this — no glob can match inline test modules.
    pub paths: Vec<String>,
    /// ast-grep rule IDs that must scan the FULL tree — tests included —
    /// bypassing [`Self::paths`]. Hard-correctness bans (e.g. `arch.no-direct-time`)
    /// where test code is NOT exempt: routing time through one primitive only
    /// works if tests obey it too. Run in a second ast-grep pass per rule with
    /// no exclude globs; the rule's own `files:` / `ignores:` scope it.
    pub scan_all_rules: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectIdentity {
    /// Used in human-facing labels: health report title, temp-log prefix.
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Backend groups a crate refuses to be built without.
    pub feature_invariants: Vec<FeatureInvariant>,
    /// Crates excluded from the `cargo hack --feature-powerset` stage.
    pub feature_powerset_exclude: Vec<String>,
    /// Crates excluded from whole-workspace stages (semver, nextest, doc-test).
    pub workspace_exclude: Vec<String>,
}

/// A rule some crates state with `compile_error!`: this build needs a backend.
///
/// The crate it applies to is not named here. It is found by the feature that
/// names the group, so the rule follows the workspace rather than being
/// repeated beside it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureInvariant {
    /// Feature whose presence marks a crate as carrying this rule.
    pub when_feature: String,
    /// Features every combination carries. A group of one is expressed here:
    /// `--at-least-one-of` needs two or more names, and where only one backend
    /// survives its target gate there is nothing to choose between.
    pub always: Vec<String>,
    /// Groups the powerset must pick from rather than leave empty.
    pub at_least_one_of: Vec<Vec<String>>,
}

impl FeatureInvariant {
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for group in &self.at_least_one_of {
            args.push("--at-least-one-of".to_owned());
            args.push(group.join(","));
        }
        if !self.always.is_empty() {
            args.push("--features".to_owned());
            args.push(self.always.join(","));
        }
        args
    }
}

#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct PerfConfig {
    pub frame_prefix: Option<String>,
    #[serde(default = "default_perf_nextest_profile")]
    pub nextest_profile: String,
    pub primary_lane: String,
    pub lanes: Vec<PerfLane>,
}

impl Default for PerfConfig {
    fn default() -> Self {
        Self {
            lanes: Vec::new(),
            primary_lane: String::new(),
            frame_prefix: None,
            nextest_profile: default_perf_nextest_profile(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct PerfLane {
    pub backend: String,
    pub flash: bool,
}

fn default_perf_nextest_profile() -> String {
    "perf".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestCommandConfig {
    pub lanes: BTreeMap<String, TestLaneConfig>,
    pub net_backends: BTreeMap<String, TestNetBackendConfig>,
    pub default_backend: String,
    pub default_lane: String,
    pub feature_arg: String,
    pub loom_lane: String,
    pub flash: TestFlashConfig,
    pub no_block: TestNoBlockConfig,
    pub features: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestFlashConfig {
    pub features: Vec<String>,
    pub default: bool,
}

impl Default for TestFlashConfig {
    fn default() -> Self {
        Self {
            features: Vec::new(),
            default: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestNoBlockConfig {
    pub features: Vec<String>,
    pub default: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestNetBackendConfig {
    pub features: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestLaneConfig {
    pub default_flash: Option<bool>,
    /// Poll-blocking detector default for this lane, so two schedulers cannot
    /// run the same lane under different rules.
    pub default_no_block: Option<bool>,
    pub passthrough: String,
    pub program: String,
    pub default_features: Vec<String>,
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
}

impl ProjectConfig {
    /// Load project-specific xtask settings from `.config/xtask.toml`.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be read or parsed.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(CONFIG_REL);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read project config: {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parse project config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for pattern in self
            .architecture
            .filters
            .exclude_crates
            .iter()
            .chain(&self.architecture.filters.exclude_modules)
        {
            if pattern.is_empty() {
                bail!("architecture exclusion glob cannot be empty");
            }
            glob::Pattern::new(pattern)
                .with_context(|| format!("invalid architecture exclusion glob `{pattern}`"))?;
        }
        let mut names = std::collections::BTreeSet::new();
        for scenario in &self.architecture.runtime.scenarios {
            scenario.validate()?;
            if !names.insert(scenario.name()) {
                bail!(
                    "duplicate architecture runtime scenario `{}`",
                    scenario.name()
                );
            }
        }
        let mut stage_names = std::collections::BTreeSet::new();
        let mut stage_tools = std::collections::BTreeSet::new();
        for stage in &self.quality.assessment.deep_stages {
            if stage.name.is_empty()
                || !stage.name.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                bail!(
                    "quality assessment stage name `{}` must use only ASCII letters, digits, '-' or '_'",
                    stage.name
                );
            }
            if !stage_names.insert(&stage.name) {
                bail!("duplicate quality assessment stage `{}`", stage.name);
            }
            if stage.command.is_empty() {
                bail!(
                    "quality assessment stage `{}` requires a command",
                    stage.name
                );
            }
            if stage.tools.is_empty() {
                bail!(
                    "quality assessment stage `{}` requires at least one owned tool signal",
                    stage.name
                );
            }
            stage_tools.extend(stage.tools.iter().map(String::as_str));
        }
        let mut policy_tools = std::collections::BTreeSet::new();
        for policy in &self.quality.assessment.not_applicable_tools {
            if policy.tool.is_empty()
                || !policy.tool.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                bail!(
                    "quality assessment policy tool `{}` must use only ASCII letters, digits, '-' or '_'",
                    policy.tool
                );
            }
            if policy.reason.trim().is_empty() {
                bail!(
                    "quality assessment policy tool `{}` requires a reason",
                    policy.tool
                );
            }
            if !policy_tools.insert(policy.tool.as_str()) {
                bail!("duplicate quality assessment policy tool `{}`", policy.tool);
            }
            if stage_tools.contains(policy.tool.as_str()) {
                bail!(
                    "quality assessment tool `{}` cannot be both configured and not applicable",
                    policy.tool
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod architecture_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn load(text: &str) -> Result<ProjectConfig> {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("config dir");
        fs::write(temp.path().join(CONFIG_REL), text).expect("config");
        ProjectConfig::load(temp.path())
    }

    #[test]
    fn architecture_runtime_config_is_strict() {
        let error = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "test"
package = "demo"
test = "architecture"
timeout_secs = 30
unknown = true
"#,
        )
        .expect_err("unknown field");

        assert!(error.to_string().contains("parse project config"));
    }

    #[test]
    fn architecture_runtime_names_and_timeouts_are_validated() {
        let duplicate = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "trace"
path = "first.jsonl"

[[architecture.runtime.scenarios]]
name = "demo"
command = "trace"
path = "second.jsonl"
"#,
        )
        .expect_err("duplicate");
        assert!(duplicate.to_string().contains("duplicate"));

        let zero = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "test"
package = "demo"
test = "architecture"
timeout_secs = 0
"#,
        )
        .expect_err("zero timeout");
        assert!(zero.to_string().contains("must be positive"));
    }

    #[test]
    fn architecture_filters_are_strict_and_validate_globs() {
        let config = load(
            r#"
[architecture.filters]
exclude_crates = ["dev-*"]
exclude_modules = ["*::tests"]
"#,
        )
        .expect("architecture filters");
        assert_eq!(
            config.architecture.filters.exclude_crates,
            ["dev-*".to_string()]
        );
        assert_eq!(
            config.architecture.filters.exclude_modules,
            ["*::tests".to_string()]
        );

        let invalid = load(
            r#"
[architecture.filters]
exclude_crates = ["["]
"#,
        )
        .expect_err("invalid glob");
        assert!(
            invalid
                .to_string()
                .contains("invalid architecture exclusion glob")
        );

        let empty = load(
            r#"
[architecture.filters]
exclude_modules = [""]
"#,
        )
        .expect_err("empty glob");
        assert!(empty.to_string().contains("cannot be empty"));

        let unknown = load(
            r#"
[architecture.filters]
unknown = ["dev-*"]
"#,
        )
        .expect_err("unknown filter field");
        assert!(unknown.to_string().contains("parse project config"));
    }

    #[test]
    fn quality_assessment_deep_stages_are_loaded() {
        let config = load(
            r#"
[[quality.assessment.not_applicable_tools]]
tool = "cargo-mutants"
reason = "not actionable for this workspace"

[[quality.assessment.deep_stages]]
name = "mutation"
command = ["just", "test", "mutants", "ci", "target/mutants"]
tools = ["mutation-runner"]
expected_artifacts = ["target/mutants/mutants.out/outcomes.json"]
hard_invariant = true
complete_only = true
"#,
        )
        .expect("quality assessment config");

        let stage = &config.quality.assessment.deep_stages[0];
        assert_eq!(stage.name, "mutation");
        assert_eq!(stage.command[0], "just");
        assert_eq!(stage.tools, ["mutation-runner"]);
        assert!(stage.hard_invariant);
        assert!(stage.complete_only);
        let policy = &config.quality.assessment.not_applicable_tools[0];
        assert_eq!(policy.tool, "cargo-mutants");
        assert_eq!(policy.reason, "not actionable for this workspace");
    }

    #[test]
    fn quality_assessment_tool_cannot_be_configured_and_not_applicable() {
        let error = load(
            r#"
[[quality.assessment.not_applicable_tools]]
tool = "cargo-mutants"
reason = "not actionable for this workspace"

[[quality.assessment.deep_stages]]
name = "mutation"
command = ["cargo", "mutants"]
tools = ["cargo-mutants"]
"#,
        )
        .expect_err("conflicting tool policy");

        assert!(
            error
                .to_string()
                .contains("cannot be both configured and not applicable")
        );
    }
}
