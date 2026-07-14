use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
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

/// Extended advisory clippy lints for the opt-in `cargo xtask audit-clippy` sweep.
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
    /// Trait directory whose every `pub trait` must carry workspace mock coverage.
    pub unimock_traits_dir: String,
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
    /// Crates excluded from the `cargo hack --feature-powerset` stage.
    pub feature_powerset_exclude: Vec<String>,
    /// Crates excluded from whole-workspace stages (semver, nextest, doc-test).
    pub workspace_exclude: Vec<String>,
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
    pub features: Vec<String>,
    pub lanes: BTreeMap<String, TestLaneConfig>,
    pub net_backends: BTreeMap<String, TestNetBackendConfig>,
    pub default_backend: String,
    pub default_lane: String,
    pub feature_arg: String,
    pub flash: TestFlashConfig,
    pub no_block: TestNoBlockConfig,
    pub loom_lane: String,
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
        toml::from_str(&text).with_context(|| format!("parse project config: {}", path.display()))
    }
}
