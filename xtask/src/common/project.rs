use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_REL: &str = ".config/xtask.toml";

/// Project-specific identity and per-tool settings for the otherwise
/// project-agnostic xtask. Loaded from `.config/xtask.toml`; every field
/// defaults to empty so a fresh project starts with no baked-in names and
/// fills in only what it uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProjectConfig {
    pub(crate) project: ProjectIdentity,
    pub(crate) health: HealthConfig,
    pub(crate) publish: PublishConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) lint_exclude: LintExcludeConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LintExcludeConfig {
    /// Workspace-relative globs whose violations are dropped from every lint
    /// namespace (`arch`, `style`, `idioms`) so baselines measure production
    /// debt, not test code. `#[cfg(test)]` blocks are stripped automatically
    /// (AST) on top of this — no glob can match inline test modules.
    pub(crate) paths: Vec<String>,
    /// Inline-module names / `::`-paths whose violations are dropped from every
    /// lint namespace, regardless of file.
    pub(crate) modules: Vec<String>,
    /// ast-grep rule IDs that must scan the FULL tree — tests included —
    /// bypassing [`Self::paths`]. Hard-correctness bans (e.g. `arch.no-direct-time`)
    /// where test code is NOT exempt: routing time through one primitive only
    /// works if tests obey it too. Run in a second ast-grep pass per rule with
    /// no exclude globs; the rule's own `files:` / `ignores:` scope it.
    pub(crate) scan_all_rules: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProjectIdentity {
    /// Used in human-facing labels: health report title, temp-log prefix.
    pub(crate) name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HealthConfig {
    /// Crates excluded from the `cargo hack --feature-powerset` stage.
    pub(crate) feature_powerset_exclude: Vec<String>,
    /// Crates excluded from whole-workspace stages (semver, nextest, doc-test).
    pub(crate) workspace_exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReleaseConfig {
    /// GitHub repo (`owner/name`) that hosts the canonical releases.
    pub(crate) github_repo: String,
    /// Self-hosted `GitLab` instance that mirrors release artifacts.
    pub(crate) gitlab_host: String,
    /// `GitLab` project: numeric id or `group/name` path.
    pub(crate) gitlab_project: String,
    /// Generic package name in the `GitLab` registry.
    pub(crate) gitlab_package: String,
    /// Primary release asset: the SPM Rust `XCFramework` zip.
    pub(crate) asset: String,
    /// Optional single self-contained framework zip (`CocoaPods` + manual
    /// drag-in). Empty disables those channels.
    pub(crate) single_asset: String,
    /// Optional `CocoaPods` podspec file name; stamped on prepare and
    /// published (GitHub + `GitLab` mirror + trunk) on publish. Empty
    /// disables the `CocoaPods` channel.
    pub(crate) podspec: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PublishConfig {
    /// Generated workspace-hack crate, stripped from published manifests.
    pub(crate) workspace_hack_crate: String,
    /// User-agent sent to the registry when checking crate availability.
    pub(crate) user_agent: String,
}

impl ProjectConfig {
    pub(crate) fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(CONFIG_REL);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read project config: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse project config: {}", path.display()))
    }
}
