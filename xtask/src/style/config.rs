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
    #[serde(default)]
    pub(crate) trait_item_order: TraitItemOrderConfig,
    #[serde(default)]
    pub(crate) struct_init_order: StructInitOrderConfig,
    #[serde(default)]
    pub(crate) comment_hygiene: CommentHygieneConfig,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TraitItemOrderConfig {
    /// Item kinds in the order they should appear inside a `trait` / `impl`.
    /// Recognised tokens: `type`, `const`, `fn`, `macro`.
    #[serde(default = "default_trait_kind_order")]
    pub(crate) kind_order: Vec<String>,
    /// Where to apply the rule: any combination of `trait` / `impl_inherent`
    /// / `impl_trait`. Defaults cover both kinds of `impl` so methods stay
    /// grouped near their associated types/consts.
    #[serde(default = "default_trait_apply_to")]
    pub(crate) apply_to: Vec<String>,
    /// Function names that must appear *first* within the `fn` kind bucket,
    /// in the listed order. Conventional constructors (`new`) come before the
    /// rest of the impl so a reader sees creation entry points up top.
    #[serde(default = "default_priority_fn_names")]
    pub(crate) priority_fn_names: Vec<String>,
}

impl Default for TraitItemOrderConfig {
    fn default() -> Self {
        Self {
            kind_order: default_trait_kind_order(),
            apply_to: default_trait_apply_to(),
            priority_fn_names: default_priority_fn_names(),
        }
    }
}

fn default_trait_kind_order() -> Vec<String> {
    ["type", "const", "fn", "macro"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn default_trait_apply_to() -> Vec<String> {
    ["trait", "impl_inherent", "impl_trait"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn default_priority_fn_names() -> Vec<String> {
    ["new"].iter().map(|s| (*s).to_string()).collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructInitOrderConfig {
    /// Whether shorthand fields (`Foo { x, y, .. }`) must precede explicit
    /// fields (`Foo { z: expr }`).
    #[serde(default = "default_shorthand_first")]
    pub(crate) shorthand_first: bool,
}

impl Default for StructInitOrderConfig {
    fn default() -> Self {
        Self {
            shorthand_first: default_shorthand_first(),
        }
    }
}

fn default_shorthand_first() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentHygieneConfig {
    /// Maximum number of consecutive lines a whitelisted inline `//` block
    /// may span before flagging `size:inline`. Once a comment spans more
    /// lines than this it likely belongs in a doc-block above the item or
    /// in the crate `README.md`.
    #[serde(default = "default_inline_max_lines")]
    pub(crate) inline_max_lines: usize,
    /// Maximum number of consecutive lines a `///` or `//!` doc-block may
    /// span before flagging `size:doc`. Long contracts belong in the
    /// owning crate `README.md` per AGENTS.md.
    #[serde(default = "default_doc_block_max_lines")]
    pub(crate) doc_block_max_lines: usize,
    /// Density threshold in percent (0..=100): a fn body where the share
    /// of non-doc inline `//` lines strictly exceeds this is flagged.
    #[serde(default = "default_fn_density_threshold_pct")]
    pub(crate) fn_density_threshold_pct: u32,
    /// Functions with body shorter than this many lines are exempt from
    /// the density check (signal would be noisy on tiny helpers).
    #[serde(default = "default_fn_density_min_body_lines")]
    pub(crate) fn_density_min_body_lines: usize,
    /// Prefixes that mark an inline `//` comment as intentional and
    /// exempt from the `category` check. Match is case-sensitive on the
    /// trimmed comment body.
    #[serde(default = "default_allowed_inline_markers")]
    pub(crate) allowed_inline_markers: Vec<String>,
    /// Workspace-relative glob patterns that opt files out of every
    /// `comment_hygiene` sub-check.
    #[serde(default = "default_exclude_paths")]
    pub(crate) exclude_paths: Vec<String>,
}

impl Default for CommentHygieneConfig {
    fn default() -> Self {
        Self {
            inline_max_lines: default_inline_max_lines(),
            doc_block_max_lines: default_doc_block_max_lines(),
            fn_density_threshold_pct: default_fn_density_threshold_pct(),
            fn_density_min_body_lines: default_fn_density_min_body_lines(),
            allowed_inline_markers: default_allowed_inline_markers(),
            exclude_paths: default_exclude_paths(),
        }
    }
}

fn default_inline_max_lines() -> usize {
    3
}

fn default_doc_block_max_lines() -> usize {
    20
}

fn default_fn_density_threshold_pct() -> u32 {
    30
}

fn default_fn_density_min_body_lines() -> usize {
    6
}

fn default_allowed_inline_markers() -> Vec<String> {
    [
        "SAFETY:",
        "TODO:",
        "FIXME:",
        "XXX:",
        "NOTE:",
        "WHY:",
        "HACK:",
        "kithara:",
        "ast-grep-ignore:",
        "xtask-lint-ignore:",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn default_exclude_paths() -> Vec<String> {
    ["**/build.rs", "**/tests/**/fixtures/**"]
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
        .with_context(|| format!("read style config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse style config: {}", path.display()))
}
