//! Loaders for `.config/style/*.toml`.
//!
//! Each style check has its own typed section in `thresholds.toml`. Missing
//! files default to empty, mirroring `arch::config` behavior.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default)]
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
pub(crate) struct ThresholdsConfig {
    #[serde(default)]
    pub(crate) struct_field_order: StructFieldOrderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructFieldOrderConfig {
    /// Visibility group order. Each field is bucketed by visibility, then
    /// sorted by type name, then by field name within the bucket.
    /// Recognised tokens: `pub`, `pub(crate)`, `pub(super)`, `pub(in)`, `private`.
    #[serde(default = "default_visibility_order")]
    pub(crate) visibility_order: Vec<String>,
    /// Outer-attribute names that exempt a struct from ordering checks.
    /// `repr` covers `#[repr(C)]`, `#[repr(packed)]`, etc., where field order
    /// is part of the layout contract.
    #[serde(default = "default_exempt_attrs")]
    pub(crate) exempt_attrs: Vec<String>,
}

impl Default for StructFieldOrderConfig {
    fn default() -> Self {
        Self {
            visibility_order: default_visibility_order(),
            exempt_attrs: default_exempt_attrs(),
        }
    }
}

fn default_visibility_order() -> Vec<String> {
    ["pub", "pub(crate)", "pub(super)", "pub(in)", "private"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn default_exempt_attrs() -> Vec<String> {
    ["repr"].iter().map(|s| (*s).to_string()).collect()
}

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
