//! Loaders for `.config/idioms/*.toml`.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default)]
pub(crate) struct IdiomsConfig {
    pub(crate) thresholds: ThresholdsConfig,
}

impl IdiomsConfig {
    pub(crate) fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            thresholds: load_optional(&dir.join("thresholds.toml"))?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThresholdsConfig {
    #[serde(default)]
    pub(crate) branch_chains: BranchChainsConfig,
    #[serde(default)]
    pub(crate) guard_cascade: GuardCascadeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BranchChainsConfig {
    /// Threshold for homogeneous chains (all arms test the same expression
    /// with the same operator) — strongest match-conversion hint.
    #[serde(default = "default_homogeneous_arms")]
    pub(crate) homogeneous_arms: usize,
    /// Threshold for heterogeneous chains (arms test unrelated predicates).
    /// Hint: extract predicates or build a tag enum + match.
    #[serde(default = "default_heterogeneous_arms")]
    pub(crate) heterogeneous_arms: usize,
    /// Threshold for any chain regardless of structure — generic cognitive-load
    /// signal.
    #[serde(default = "default_general_arms")]
    pub(crate) general_arms: usize,
    /// If false, `if let` chains are excluded (they often have no `match` form).
    #[serde(default)]
    pub(crate) count_if_let: bool,
    /// Glob patterns relative to the workspace root that exempt a file from
    /// the check.
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for BranchChainsConfig {
    fn default() -> Self {
        Self {
            homogeneous_arms: default_homogeneous_arms(),
            heterogeneous_arms: default_heterogeneous_arms(),
            general_arms: default_general_arms(),
            count_if_let: false,
            exempt_files: default_exempt_files(),
        }
    }
}

fn default_homogeneous_arms() -> usize {
    3
}
fn default_heterogeneous_arms() -> usize {
    5
}
fn default_general_arms() -> usize {
    6
}
fn default_exempt_files() -> Vec<String> {
    ["**/tests/**", "**/benches/**"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuardCascadeConfig {
    /// Threshold for consecutive guard statements (early-return ifs and
    /// let-else) inside one block to flag the block as a guard cascade.
    #[serde(default = "default_cascade_warn_streak")]
    pub(crate) warn_streak: usize,
    /// Macro idents that count as terminators in the guard body
    /// (`panic!()`, `bail!()`, …).
    #[serde(default = "default_terminator_macros")]
    pub(crate) terminator_macros: Vec<String>,
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for GuardCascadeConfig {
    fn default() -> Self {
        Self {
            warn_streak: default_cascade_warn_streak(),
            terminator_macros: default_terminator_macros(),
            exempt_files: default_exempt_files(),
        }
    }
}

fn default_cascade_warn_streak() -> usize {
    4
}

fn default_terminator_macros() -> Vec<String> {
    [
        "panic",
        "bail",
        "todo",
        "unreachable",
        "unimplemented",
        "ensure",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn load_optional<T>(path: &Path) -> Result<T>
where
    T: Default + for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("read idioms config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse idioms config: {}", path.display()))
}
