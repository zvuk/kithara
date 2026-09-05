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
    pub(crate) comment_hygiene: CommentHygieneConfig,
    #[serde(default)]
    pub(crate) dead_doc_refs: DeadDocRefsConfig,
    #[serde(default)]
    pub(crate) doc_size: DocSizeConfig,
    #[serde(default)]
    pub(crate) doc_staleness: DocStalenessConfig,
    #[serde(default)]
    pub(crate) non_english_text: NonEnglishTextConfig,
    #[serde(default)]
    pub(crate) readme_shape: ReadmeShapeConfig,
    #[serde(default)]
    pub(crate) struct_field_order: StructFieldOrderConfig,
    #[serde(default)]
    pub(crate) struct_init_order: StructInitOrderConfig,
    #[serde(default)]
    pub(crate) trait_item_order: TraitItemOrderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructFieldOrderConfig {
    /// Outer-attribute names that exempt a struct from ordering checks.
    /// `repr` covers `#[repr(C)]`, `#[repr(packed)]`, etc., where field order
    /// is part of the layout contract.
    #[serde(default = "default_exempt_attrs")]
    pub(crate) exempt_attrs: Vec<String>,
    /// Visibility group order. Each field is bucketed by visibility, then
    /// sorted by type name, then by field name within the bucket.
    /// Recognised tokens: `pub`, `pub(crate)`, `pub(super)`, `pub(in)`, `private`.
    #[serde(default = "default_visibility_order")]
    pub(crate) visibility_order: Vec<String>,
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
    /// Where to apply the rule: any combination of `trait` / `impl_inherent`
    /// / `impl_trait`. Defaults cover both kinds of `impl` so methods stay
    /// grouped near their associated types/consts.
    #[serde(default = "default_trait_apply_to")]
    pub(crate) apply_to: Vec<String>,
    /// Item kinds in the order they should appear inside a `trait` / `impl`.
    /// Recognised tokens: `type`, `const`, `fn`, `macro`.
    #[serde(default = "default_trait_kind_order")]
    pub(crate) kind_order: Vec<String>,
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
    /// Rewrite passes the fix makes over one file before it gives up.
    /// Reordering a literal moves the literals nested inside it, so each pass
    /// exposes the next nesting level and a file needs as many passes as it
    /// has levels.
    #[serde(default = "default_max_fix_passes")]
    pub(crate) max_fix_passes: usize,
}

impl Default for StructInitOrderConfig {
    fn default() -> Self {
        Self {
            shorthand_first: default_shorthand_first(),
            max_fix_passes: default_max_fix_passes(),
        }
    }
}

const fn default_shorthand_first() -> bool {
    true
}

const fn default_max_fix_passes() -> usize {
    8
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentHygieneConfig {
    /// Prefixes that keep an inline `//` comment. These are machine and
    /// language markup, not prose: a tool directive, or the safety note the
    /// compiler's own convention puts on an `unsafe` block. Prose belongs in a
    /// doc comment, so no prose marker is listed here. Match is case-sensitive
    /// on the trimmed comment body.
    #[serde(default = "default_allowed_inline_markers")]
    pub(crate) allowed_inline_markers: Vec<String>,
    /// Workspace-relative glob patterns that opt files out of every
    /// `comment_hygiene` sub-check.
    #[serde(default = "default_exclude_paths")]
    pub(crate) exclude_paths: Vec<String>,
    /// Annotations an author reached for instead of writing documentation. A
    /// block carrying one is never promoted to `///`: publishing `WHY:` as
    /// rendered docs would launder the note rather than answer it, so it stays
    /// for a human.
    #[serde(default = "default_prose_markers")]
    pub(crate) prose_markers: Vec<String>,
    /// Density threshold in percent (0..=100): a fn body where the share
    /// of non-doc inline `//` lines strictly exceeds this is flagged.
    #[serde(default = "default_fn_density_threshold_pct")]
    pub(crate) fn_density_threshold_pct: u32,
    /// Maximum number of consecutive lines a `///` or `//!` doc-block may
    /// span before flagging `size:doc`. Documentation earns its place by being
    /// dense; past a dozen lines it is a document, and a document belongs in
    /// the owning crate `README.md` per AGENTS.md.
    #[serde(default = "default_doc_block_max_lines")]
    pub(crate) doc_block_max_lines: usize,
    /// Functions with body shorter than this many lines are exempt from
    /// the density check (signal would be noisy on tiny helpers).
    #[serde(default = "default_fn_density_min_body_lines")]
    pub(crate) fn_density_min_body_lines: usize,
    /// Maximum number of consecutive lines a whitelisted inline `//` block
    /// may span before flagging `size:inline`. Once a comment spans more
    /// lines than this it likely belongs in a doc-block above the item or
    /// in the crate `README.md`.
    #[serde(default = "default_inline_max_lines")]
    pub(crate) inline_max_lines: usize,
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
            prose_markers: default_prose_markers(),
        }
    }
}

const fn default_inline_max_lines() -> usize {
    3
}

const fn default_doc_block_max_lines() -> usize {
    12
}

const fn default_fn_density_threshold_pct() -> u32 {
    30
}

const fn default_fn_density_min_body_lines() -> usize {
    6
}

fn default_allowed_inline_markers() -> Vec<String> {
    ["SAFETY:", "ast-grep-ignore:", "xtask-lint-ignore:"]
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

fn default_prose_markers() -> Vec<String> {
    ["WHY:", "NOTE:", "TODO:", "FIXME:", "XXX:", "HACK:"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocSizeConfig {
    /// Documents excluded from the size limits entirely.
    #[serde(default)]
    pub(crate) exclude_paths: Vec<String>,
    /// Per-class limits. The first rule whose globs match the document wins.
    #[serde(default)]
    pub(crate) limits: Vec<DocSizeLimit>,
}

/// Documented identifiers that no longer exist in the workspace sources.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocStalenessConfig {
    /// Backticked terms that are prose or external names, not workspace code.
    #[serde(default)]
    pub(crate) allow_terms: Vec<String>,
    /// Documents excluded from the check entirely.
    #[serde(default)]
    pub(crate) exclude_paths: Vec<String>,
    /// Documents the check reads.
    #[serde(default)]
    pub(crate) include_globs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocSizeLimit {
    /// Documents this rule applies to.
    pub(crate) globs: Vec<String>,
    /// Byte count above which the document is denied. A document costs an
    /// agent its size, not its line count.
    pub(crate) deny: usize,
    /// Byte count above which the document is reported.
    pub(crate) warn: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NonEnglishTextConfig {
    /// Workspace-relative glob patterns that opt paths out of the tracked text
    /// scan. Binary payload directories and local-only planning docs live here
    /// as configuration, not baked-in check constants.
    #[serde(default = "default_non_english_exclude_paths")]
    pub(crate) exclude_paths: Vec<String>,
    /// Characters of the offending line quoted in the violation message before
    /// it is elided.
    #[serde(default = "default_excerpt_chars")]
    pub(crate) excerpt_chars: usize,
}

impl Default for NonEnglishTextConfig {
    fn default() -> Self {
        Self {
            exclude_paths: default_non_english_exclude_paths(),
            excerpt_chars: default_excerpt_chars(),
        }
    }
}

fn default_non_english_exclude_paths() -> Vec<String> {
    default_tracked_text_exclude_paths()
}

const fn default_excerpt_chars() -> usize {
    120
}

/// The one shape every crate README follows.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadmeShapeConfig {
    /// Documents excluded from the shape contract entirely.
    #[serde(default)]
    pub(crate) exclude_paths: Vec<String>,
    /// Documents the check reads.
    #[serde(default = "default_readme_include_globs")]
    pub(crate) include_globs: Vec<String>,
    /// Top-level sections a crate README may carry, in the order they must
    /// appear. Every one is optional; anything outside this vocabulary is a
    /// `###` subsection of one of them, so a reader who knows one README
    /// knows where to look in all of them.
    #[serde(default = "default_readme_sections")]
    pub(crate) sections: Vec<String>,
}

impl Default for ReadmeShapeConfig {
    fn default() -> Self {
        Self {
            include_globs: default_readme_include_globs(),
            sections: default_readme_sections(),
            exclude_paths: Vec::new(),
        }
    }
}

fn default_readme_include_globs() -> Vec<String> {
    ["crates/*/README.md"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn default_readme_sections() -> Vec<String> {
    ["Usage", "Key Types", "Features", "Integration"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeadDocRefsConfig {
    /// Workspace-relative referenced target globs allowed to be local-only.
    #[serde(default)]
    pub(crate) allow_targets: Vec<String>,
    /// Workspace-relative glob patterns that opt paths out of the tracked text
    /// scan. Defaults match `non_english_text` so generated, binary, and local
    /// planning trees stay configuration-owned.
    #[serde(default = "default_dead_doc_refs_exclude_paths")]
    pub(crate) exclude_paths: Vec<String>,
}

impl Default for DeadDocRefsConfig {
    fn default() -> Self {
        Self {
            exclude_paths: default_dead_doc_refs_exclude_paths(),
            allow_targets: Vec::new(),
        }
    }
}

fn default_dead_doc_refs_exclude_paths() -> Vec<String> {
    let mut paths = default_tracked_text_exclude_paths();
    // Lint-check implementations carry synthetic fixture paths in their
    // unit tests; those strings are test vectors, not references.
    paths.push("**/kithara-devtools/src/style/checks/*.rs".to_string());
    paths
}

fn default_tracked_text_exclude_paths() -> Vec<String> {
    [
        "assets/**",
        "**/assets/**",
        "fonts/**",
        "**/fonts/**",
        "models/**",
        "**/models/**",
        "fixtures/**",
        "**/fixtures/**",
        "target/**",
        "**/target/**",
        "docs/plans/**",
        "docs/specs/**",
        "android/gradle/wrapper/**",
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
        .with_context(|| format!("read style config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse style config: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moved_style_defaults_match_the_constants_they_replace() {
        let thresholds = ThresholdsConfig::default();

        assert_eq!(
            thresholds.comment_hygiene.prose_markers,
            ["WHY:", "NOTE:", "TODO:", "FIXME:", "XXX:", "HACK:"]
        );
        assert_eq!(thresholds.non_english_text.excerpt_chars, 120);
        assert_eq!(thresholds.struct_init_order.max_fix_passes, 8);
    }

    /// The other half of the same trap. A present table is filled key by key
    /// from the per-field defaults and never from the written `Default`, so a
    /// project that sets one key of a check must keep the check's other
    /// subjects.
    #[test]
    fn a_partly_configured_check_keeps_the_keys_it_left_alone() {
        let thresholds: ThresholdsConfig = toml::from_str(
            r#"
[comment_hygiene]
inline_max_lines = 5

[non_english_text]
exclude_paths = ["docs/local/"]

[struct_init_order]
shorthand_first = false
"#,
        )
        .expect("a partial style document");

        assert_eq!(thresholds.comment_hygiene.inline_max_lines, 5);
        assert_eq!(
            thresholds.comment_hygiene.prose_markers,
            ["WHY:", "NOTE:", "TODO:", "FIXME:", "XXX:", "HACK:"]
        );
        assert_eq!(thresholds.non_english_text.excerpt_chars, 120);
        assert!(!thresholds.struct_init_order.shorthand_first);
        assert_eq!(thresholds.struct_init_order.max_fix_passes, 8);
    }
}
