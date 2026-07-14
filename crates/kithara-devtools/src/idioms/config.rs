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
    pub(crate) accumulator_loops: AccumulatorLoopsConfig,
    #[serde(default)]
    pub(crate) arc_mutex_collection: ArcMutexCollectionConfig,
    #[serde(default)]
    pub(crate) await_under_guard: AwaitUnderGuardConfig,
    #[serde(default)]
    pub(crate) box_concrete_type: BoxConcreteTypeConfig,
    #[serde(default)]
    pub(crate) branch_chains: BranchChainsConfig,
    #[serde(default)]
    pub(crate) const_group_enum_shape: ConstGroupEnumShapeConfig,
    #[serde(default)]
    pub(crate) derivable_delegation: DerivableDelegationConfig,
    #[serde(default)]
    pub(crate) derivable_display: DerivableConfig,
    #[serde(default)]
    pub(crate) derivable_deref: DerivableConfig,
    #[serde(default)]
    pub(crate) derivable_from: DerivableConfig,
    #[serde(default)]
    pub(crate) derivable_getter: DerivableConfig,
    #[serde(default)]
    pub(crate) fat_loop_body: FatLoopBodyConfig,
    #[serde(default)]
    pub(crate) function_branch_density: FunctionBranchDensityConfig,
    #[serde(default)]
    pub(crate) guard_cascade: GuardCascadeConfig,
    #[serde(default)]
    pub(crate) loop_allocation: LoopAllocationConfig,
    #[serde(default)]
    pub(crate) loop_flag_accumulator: LoopFlagAccumulatorConfig,
    #[serde(default)]
    pub(crate) manual_question_mark: ManualQuestionMarkConfig,
    #[serde(default)]
    pub(crate) multi_accumulator_loop: MultiAccumulatorLoopConfig,
    #[serde(default)]
    pub(crate) nested_if_let_pyramid: NestedIfLetPyramidConfig,
    #[serde(default)]
    pub(crate) no_passthrough_builder: NoPassthroughBuilderConfig,
    #[serde(default)]
    pub(crate) parallel_loops: ParallelLoopsConfig,
    #[serde(default)]
    pub(crate) pointwise_loop: PointwiseLoopConfig,
    #[serde(default)]
    pub(crate) retry_fallback: RetryFallbackConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DerivableConfig {
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl Default for DerivableConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DerivableDelegationConfig {
    #[serde(default = "default_delegation_enabled")]
    pub(crate) enabled: bool,
    #[serde(default = "default_trait_delegation_methods")]
    pub(crate) trait_min_methods: usize,
    #[serde(default = "default_inherent_delegation_methods")]
    pub(crate) inherent_min_methods: usize,
    #[serde(default = "default_blocking_impl_attrs")]
    pub(crate) blocking_impl_attrs: Vec<String>,
    #[serde(default = "default_keep_manual_method_attrs")]
    pub(crate) keep_manual_method_attrs: Vec<String>,
}

fn default_delegation_enabled() -> bool {
    true
}

fn default_trait_delegation_methods() -> usize {
    2
}

fn default_inherent_delegation_methods() -> usize {
    2
}

fn default_blocking_impl_attrs() -> Vec<String> {
    vec![
        "async_trait".into(),
        "uniffi::export".into(),
        "wasm_bindgen".into(),
    ]
}

fn default_keep_manual_method_attrs() -> Vec<String> {
    Vec::new()
}

impl Default for DerivableDelegationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trait_min_methods: default_trait_delegation_methods(),
            inherent_min_methods: default_inherent_delegation_methods(),
            blocking_impl_attrs: default_blocking_impl_attrs(),
            keep_manual_method_attrs: default_keep_manual_method_attrs(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PointwiseLoopConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for PointwiseLoopConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FatLoopBodyConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    #[serde(default = "default_for_stmt_threshold")]
    pub(crate) for_stmt_threshold: usize,
    #[serde(default = "default_loop_stmt_threshold")]
    pub(crate) loop_stmt_threshold: usize,
    #[serde(default = "default_nested_ctrl_threshold")]
    pub(crate) nested_ctrl_threshold: usize,
    #[serde(default = "default_while_stmt_threshold")]
    pub(crate) while_stmt_threshold: usize,
}

fn default_for_stmt_threshold() -> usize {
    6
}
fn default_while_stmt_threshold() -> usize {
    6
}
fn default_loop_stmt_threshold() -> usize {
    4
}
fn default_nested_ctrl_threshold() -> usize {
    1
}

impl Default for FatLoopBodyConfig {
    fn default() -> Self {
        Self {
            for_stmt_threshold: default_for_stmt_threshold(),
            while_stmt_threshold: default_while_stmt_threshold(),
            loop_stmt_threshold: default_loop_stmt_threshold(),
            nested_ctrl_threshold: default_nested_ctrl_threshold(),
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoopFlagAccumulatorConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for LoopFlagAccumulatorConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConstGroupEnumShapeConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    #[serde(default = "default_min_group_size")]
    pub(crate) min_group_size: usize,
    #[serde(default = "default_min_prefix_chars")]
    pub(crate) min_prefix_chars: usize,
}

fn default_min_group_size() -> usize {
    3
}
fn default_min_prefix_chars() -> usize {
    3
}

impl Default for ConstGroupEnumShapeConfig {
    fn default() -> Self {
        Self {
            min_group_size: default_min_group_size(),
            min_prefix_chars: default_min_prefix_chars(),
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NestedIfLetPyramidConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    #[serde(default = "default_pyramid_min_depth")]
    pub(crate) min_depth: usize,
}

fn default_pyramid_min_depth() -> usize {
    2
}

impl Default for NestedIfLetPyramidConfig {
    fn default() -> Self {
        Self {
            min_depth: default_pyramid_min_depth(),
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BranchChainsConfig {
    /// Glob patterns relative to the workspace root that exempt a file from
    /// the check.
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// If false, `if let` chains are excluded (they often have no `match` form).
    #[serde(default)]
    pub(crate) count_if_let: bool,
    /// Threshold for any chain regardless of structure — generic cognitive-load
    /// signal.
    #[serde(default = "default_general_arms")]
    pub(crate) general_arms: usize,
    /// Threshold for heterogeneous chains (arms test unrelated predicates).
    /// Hint: extract predicates or build a tag enum + match.
    #[serde(default = "default_heterogeneous_arms")]
    pub(crate) heterogeneous_arms: usize,
    /// Threshold for homogeneous chains (all arms test the same expression
    /// with the same operator) — strongest match-conversion hint.
    #[serde(default = "default_homogeneous_arms")]
    pub(crate) homogeneous_arms: usize,
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
    4
}
fn default_general_arms() -> usize {
    5
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
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// Macro idents that count as terminators in the guard body
    /// (`panic!()`, `bail!()`, …).
    #[serde(default = "default_terminator_macros")]
    pub(crate) terminator_macros: Vec<String>,
    /// Threshold for consecutive guard statements (early-return ifs and
    /// let-else) inside one block to flag the block as a guard cascade.
    #[serde(default = "default_cascade_warn_streak")]
    pub(crate) warn_streak: usize,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FunctionBranchDensityConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// Warn when a single function body owns more than this many branch
    /// decisions (if / match-dispatch / loop / `&&` / `||` / let-else).
    /// Density is attributed to the function that *owns* the branches, not
    /// to thin callers that merely route into dense callees. Always a
    /// warning — never escalated. Existing code that breaches the threshold
    /// is technical debt, not an accepted floor; the detector keeps firing
    /// on every audit run so the noise stays visible until restructured.
    #[serde(default = "default_density_warn_own")]
    pub(crate) warn_own: usize,
}

impl Default for FunctionBranchDensityConfig {
    fn default() -> Self {
        Self {
            warn_own: default_density_warn_own(),
            exempt_files: default_exempt_files(),
        }
    }
}

fn default_density_warn_own() -> usize {
    12
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccumulatorLoopsConfig {
    /// Patterns to detect: `push`, `extend`, `sum` (numeric `+=`/`-=`),
    /// `count` (conditional `+= 1` inside an `if`).
    #[serde(default = "default_accumulator_patterns")]
    pub(crate) detect: Vec<String>,
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// Skip loops with `break`/`continue`/`return` inside the body — converting
    /// to an iterator chain there changes control flow semantics.
    #[serde(default = "default_true")]
    pub(crate) ignore_with_break: bool,
}

impl Default for AccumulatorLoopsConfig {
    fn default() -> Self {
        Self {
            detect: default_accumulator_patterns(),
            ignore_with_break: true,
            exempt_files: default_exempt_files(),
        }
    }
}

fn default_accumulator_patterns() -> Vec<String> {
    ["push", "extend", "sum", "count"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoPassthroughBuilderConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// Minimum number of input fields routed straight through to the
    /// target builder before the function is flagged. Below this
    /// threshold a 2-3 field carrier is considered too small to be
    /// worth removing.
    #[serde(default = "default_min_passthrough_fields")]
    pub(crate) min_passthrough_fields: usize,
}

impl Default for NoPassthroughBuilderConfig {
    fn default() -> Self {
        Self {
            min_passthrough_fields: default_min_passthrough_fields(),
            exempt_files: default_exempt_files(),
        }
    }
}

fn default_min_passthrough_fields() -> usize {
    4
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MultiAccumulatorLoopConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
    /// Skip loops with `break`/`continue`/`return` inside.
    #[serde(default = "default_true")]
    pub(crate) ignore_with_break: bool,
}

impl Default for MultiAccumulatorLoopConfig {
    fn default() -> Self {
        Self {
            ignore_with_break: true,
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParallelLoopsConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for ParallelLoopsConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualQuestionMarkConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for ManualQuestionMarkConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoopAllocationConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for LoopAllocationConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoxConcreteTypeConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for BoxConcreteTypeConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArcMutexCollectionConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for ArcMutexCollectionConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AwaitUnderGuardConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for AwaitUnderGuardConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetryFallbackConfig {
    #[serde(default = "default_exempt_files")]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for RetryFallbackConfig {
    fn default() -> Self {
        Self {
            exempt_files: default_exempt_files(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_manual_method_attrs_defaults_to_empty() -> Result<()> {
        let configured: DerivableDelegationConfig = toml::from_str("")?;

        assert!(configured.keep_manual_method_attrs.is_empty());
        assert!(
            DerivableDelegationConfig::default()
                .keep_manual_method_attrs
                .is_empty()
        );
        Ok(())
    }
}
