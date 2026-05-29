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
