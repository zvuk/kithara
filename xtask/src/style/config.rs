//! Loaders for `.config/style/*.toml`.
//!
//! Each style check has its own typed section in `thresholds.toml`. Missing
//! files default to empty, mirroring `arch::config` behavior.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default)]
#[expect(dead_code, reason = "thresholds is wired in for upcoming style checks")]
pub(crate) struct StyleConfig {
    pub(crate) thresholds: ThresholdsConfig,
}

impl StyleConfig {
    pub(crate) fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            thresholds: load_optional(&dir.join("thresholds.toml"))?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThresholdsConfig {}

fn load_optional<T>(path: &Path) -> Result<T>
where
    T: Default + for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("read style config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse style config: {}", path.display()))
}
