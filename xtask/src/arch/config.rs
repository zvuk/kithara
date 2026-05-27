use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default)]
pub(crate) struct ArchConfig {
    pub(crate) direction: DirectionConfig,
    pub(crate) canonical_types: CanonicalTypesConfig,
    pub(crate) thresholds: ThresholdsConfig,
    pub(crate) module_layers: ModuleLayersConfig,
}

impl ArchConfig {
    pub(crate) fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            direction: load_optional(&dir.join("direction.toml"))?,
            canonical_types: load_optional(&dir.join("canonical-types.toml"))?,
            thresholds: load_optional(&dir.join("thresholds.toml"))?,
            module_layers: load_optional(&dir.join("module-layers.toml"))?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectionConfig {
    #[serde(default, rename = "layer")]
    pub(crate) layers: Vec<Layer>,
    #[serde(default)]
    pub(crate) exemptions: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Layer {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) crates: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalTypesConfig {
    #[serde(default, rename = "canonical")]
    pub(crate) entries: Vec<CanonicalType>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalType {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) owner: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThresholdsConfig {
    #[serde(default)]
    pub(crate) cfg_density: CfgDensityThreshold,
    #[serde(default)]
    pub(crate) file_size: FileSizeThreshold,
    #[serde(default)]
    pub(crate) file_density: FileDensityThreshold,
    #[serde(default)]
    pub(crate) shared_state: SharedStateThreshold,
    #[serde(default)]
    pub(crate) arc_clone_hotspots: ArcCloneHotspotsThreshold,
    #[serde(default)]
    pub(crate) god_module: GodModuleThreshold,
    #[serde(default)]
    pub(crate) god_struct: GodStructThreshold,
    #[serde(default)]
    pub(crate) god_trait: GodTraitThreshold,
    #[serde(default)]
    pub(crate) pub_struct_open_fields: PubStructOpenFieldsThreshold,
    #[serde(default)]
    pub(crate) trait_impl_count: TraitImplCountThreshold,
    #[serde(default)]
    pub(crate) fn_arg_count: FnArgCountThreshold,
    #[serde(default)]
    pub(crate) generic_param_count: GenericParamCountThreshold,
    #[serde(default)]
    pub(crate) no_lib_statics: NoLibStaticsThreshold,
    #[serde(default)]
    pub(crate) module_fan_out: ModuleFanOutThreshold,
    #[serde(default)]
    pub(crate) flat_directory: FlatDirectoryThreshold,
    #[serde(default)]
    pub(crate) max_nesting: MaxNestingThreshold,
    #[serde(default)]
    pub(crate) readme_presence: ReadmePresenceThreshold,
    #[serde(default)]
    pub(crate) mixed_entities: MixedEntitiesThreshold,
    #[serde(default)]
    pub(crate) redundant_accessors: RedundantAccessorsThreshold,
    #[serde(default)]
    pub(crate) single_word_filenames: SingleWordFilenamesThreshold,
    #[serde(default)]
    pub(crate) single_impl_size: SingleImplSizeThreshold,
    #[serde(default)]
    pub(crate) redundant_reexport: RedundantReexportThreshold,
    #[serde(default)]
    pub(crate) multi_constructor: MultiConstructorThreshold,
    #[serde(default)]
    pub(crate) field_passthrough: FieldPassthroughThreshold,
    #[serde(default)]
    pub(crate) args_wrapper_struct: ArgsWrapperStructThreshold,
    #[serde(default)]
    pub(crate) field_always_constant: FieldAlwaysConstantThreshold,
    #[serde(default)]
    pub(crate) field_always_equals_other_field: FieldAlwaysEqualsOtherFieldThreshold,
    #[serde(default)]
    pub(crate) cancel_hierarchy: CancelHierarchyThreshold,
    #[serde(default)]
    pub(crate) dead_exports: DeadExportsThreshold,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MultiConstructorThreshold {
    /// Names that are always considered the canonical constructor.
    #[serde(default = "default_canonical_ctor_names")]
    pub(crate) canonical_names: Vec<String>,
    #[serde(default)]
    pub(crate) exempt_files: Vec<String>,
}

fn default_canonical_ctor_names() -> Vec<String> {
    ["new", "default"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

impl Default for MultiConstructorThreshold {
    fn default() -> Self {
        Self {
            canonical_names: default_canonical_ctor_names(),
            exempt_files: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldPassthroughThreshold {
    #[serde(default)]
    pub(crate) exempt_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArgsWrapperStructThreshold {
    #[serde(default = "default_args_wrapper_min_fields")]
    pub(crate) min_fields: usize,
    #[serde(default = "default_args_wrapper_min_call_sites")]
    pub(crate) min_call_sites: usize,
    #[serde(default)]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for ArgsWrapperStructThreshold {
    fn default() -> Self {
        Self {
            min_fields: default_args_wrapper_min_fields(),
            min_call_sites: default_args_wrapper_min_call_sites(),
            exempt_files: Vec::new(),
        }
    }
}

fn default_args_wrapper_min_fields() -> usize {
    5
}

fn default_args_wrapper_min_call_sites() -> usize {
    2
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldAlwaysConstantThreshold {
    #[serde(default = "default_field_always_min_call_sites")]
    pub(crate) min_call_sites: usize,
    #[serde(default)]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for FieldAlwaysConstantThreshold {
    fn default() -> Self {
        Self {
            min_call_sites: default_field_always_min_call_sites(),
            exempt_files: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldAlwaysEqualsOtherFieldThreshold {
    #[serde(default = "default_field_always_min_call_sites")]
    pub(crate) min_call_sites: usize,
    #[serde(default)]
    pub(crate) exempt_files: Vec<String>,
}

impl Default for FieldAlwaysEqualsOtherFieldThreshold {
    fn default() -> Self {
        Self {
            min_call_sites: default_field_always_min_call_sites(),
            exempt_files: Vec::new(),
        }
    }
}

fn default_field_always_min_call_sites() -> usize {
    3
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedundantReexportThreshold {
    /// Sub-checks to run: `explicit_duplicate` (R1) and `associated_type_leak` (R2).
    #[serde(default = "default_redundant_reexport_detect")]
    pub(crate) detect: Vec<String>,
    /// Type names to exempt (canonical without crate prefix, e.g. `FileConfig`).
    #[serde(default)]
    pub(crate) exempt: Vec<String>,
}

impl Default for RedundantReexportThreshold {
    fn default() -> Self {
        Self {
            detect: default_redundant_reexport_detect(),
            exempt: Vec::new(),
        }
    }
}

fn default_redundant_reexport_detect() -> Vec<String> {
    ["explicit_duplicate", "associated_type_leak"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CfgDensityThreshold {
    pub(crate) warn: usize,
    pub(crate) deny: usize,
    #[serde(default)]
    pub(crate) exempt_crates: Vec<String>,
    #[serde(default)]
    pub(crate) exclude_globs: Vec<String>,
}

impl Default for CfgDensityThreshold {
    fn default() -> Self {
        Self {
            warn: 5,
            deny: 10,
            exempt_crates: Vec::new(),
            exclude_globs: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileSizeThreshold {
    pub(crate) warn: usize,
    pub(crate) deny: usize,
    #[serde(default)]
    pub(crate) exclude_globs: Vec<String>,
}

impl Default for FileSizeThreshold {
    fn default() -> Self {
        Self {
            warn: 400,
            deny: 1000,
            exclude_globs: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileDensityThreshold {
    pub(crate) warn_fns_per_type: f64,
    pub(crate) deny_fns_per_type: f64,
    pub(crate) min_fns_to_evaluate: usize,
}

impl Default for FileDensityThreshold {
    fn default() -> Self {
        Self {
            warn_fns_per_type: 25.0,
            deny_fns_per_type: 40.0,
            min_fns_to_evaluate: 20,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedStateThreshold {
    pub(crate) warn: usize,
    pub(crate) deny: usize,
    #[serde(default = "default_shared_state_patterns")]
    pub(crate) patterns: Vec<String>,
}

impl Default for SharedStateThreshold {
    fn default() -> Self {
        Self {
            warn: 3,
            deny: 5,
            patterns: default_shared_state_patterns(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArcCloneHotspotsThreshold {
    pub(crate) warn: usize,
}

impl Default for ArcCloneHotspotsThreshold {
    fn default() -> Self {
        Self { warn: 3 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GodModuleThreshold {
    /// Default warn threshold = number of `pub`/`pub(crate)` items in one
    /// module file that triggers a violation.
    pub(crate) warn: usize,
    /// Per-crate override map: crate-name → custom warn threshold. Lets
    /// app/test/macro crates relax the default without baselining each file.
    #[serde(default)]
    pub(crate) overrides: BTreeMap<String, usize>,
}

impl Default for GodModuleThreshold {
    fn default() -> Self {
        Self {
            warn: 8,
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GodStructThreshold {
    /// `fields + methods` per struct. Methods include both inherent and
    /// trait-impl methods aggregated from all `impl` blocks in the file.
    pub(crate) warn: usize,
}

impl Default for GodStructThreshold {
    fn default() -> Self {
        Self { warn: 15 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GodTraitThreshold {
    /// Number of method (`fn`) items in a single trait definition.
    pub(crate) warn: usize,
}

impl Default for GodTraitThreshold {
    fn default() -> Self {
        Self { warn: 7 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PubStructOpenFieldsThreshold {
    /// `pub` structs with at least this many `pub` fields are flagged. Signals
    /// missing invariants / direct mutation. Candidate for a builder or
    /// encapsulated setter API.
    pub(crate) warn: usize,
}

impl Default for PubStructOpenFieldsThreshold {
    fn default() -> Self {
        Self { warn: 3 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FnArgCountThreshold {
    /// Functions with this many arguments (excluding `self`) trigger a warn.
    /// Tighter than clippy's `too_many_arguments` default (7) — used as a
    /// map of the codebase, not an enforced gate.
    pub(crate) warn: usize,
}

impl Default for FnArgCountThreshold {
    fn default() -> Self {
        Self { warn: 5 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleFanOutThreshold {
    /// Default warn threshold = number of distinct intra-crate sibling
    /// modules a single file imports from before being flagged as an
    /// orchestrator candidate.
    pub(crate) warn: usize,
    /// Per-crate override map: crate-name → custom warn threshold.
    #[serde(default)]
    pub(crate) overrides: BTreeMap<String, usize>,
}

impl Default for ModuleFanOutThreshold {
    fn default() -> Self {
        Self {
            warn: 4,
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoLibStaticsThreshold {
    /// Crate names exempt from the rule. App / FFI / wasm / xtask / test
    /// support crates legitimately own singletons; lib crates do not.
    #[serde(default)]
    pub(crate) exempt_crates: Vec<String>,
}

impl Default for NoLibStaticsThreshold {
    fn default() -> Self {
        Self {
            exempt_crates: default_no_lib_statics_exempt_crates(),
        }
    }
}

fn default_no_lib_statics_exempt_crates() -> Vec<String> {
    [
        "kithara-app",
        "kithara-ffi",
        "xtask",
        "kithara-test-utils",
        "kithara-test-macros",
        "kithara-probe-macros",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelHierarchyThreshold {
    /// Crates whose production *is* test scaffolding (helpers, mocks).
    /// Their hard-coded `CancellationToken::new()` calls are
    /// indistinguishable from test fixtures and don't violate the
    /// hierarchy contract.
    #[serde(default = "default_cancel_hierarchy_exempt_crates")]
    pub(crate) exempt_crates: Vec<String>,
}

impl Default for CancelHierarchyThreshold {
    fn default() -> Self {
        Self {
            exempt_crates: default_cancel_hierarchy_exempt_crates(),
        }
    }
}

fn default_cancel_hierarchy_exempt_crates() -> Vec<String> {
    [
        "kithara-test-utils",
        "kithara-test-macros",
        "kithara-probe-macros",
        "xtask",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeadExportsThreshold {
    /// Definition kinds to evaluate: `fn`, `method`, `const`, `static`,
    /// `type`, `struct`, `enum`, `trait`.
    #[serde(default = "default_dead_exports_kinds")]
    pub(crate) kinds: Vec<String>,
    /// Crates treated as test-only scaffolding (integration harness, test-util
    /// and test-macro crates). Any reference originating in one of these counts
    /// as a test reference. Configured in
    /// `.config/arch/thresholds.toml` (`[dead_exports]`).
    #[serde(default)]
    pub(crate) test_crates: Vec<String>,
    /// Crates skipped entirely (neither defs collected nor refs counted):
    /// build tooling and workspace-hack shims. Configured in
    /// `.config/arch/thresholds.toml` (`[dead_exports]`).
    #[serde(default)]
    pub(crate) ignore_crates: Vec<String>,
    /// Symbol names that are never flagged (e.g. FFI entry points the scanner
    /// cannot see called from non-Rust callers).
    #[serde(default)]
    pub(crate) exempt: Vec<String>,
    /// Workspace-relative path fragments whose definitions `--fix` must never
    /// auto-delete: platform-gated module directories (e.g. `android/`) whose
    /// callers live in build configurations or non-Rust code this scan cannot
    /// see. Reporting still flags them; only autofix is held back.
    #[serde(default)]
    pub(crate) fix_protect_paths: Vec<String>,
    /// Attribute path segments that mark a definition as an external entry
    /// point with no in-tree Rust caller (`no_mangle`, `wasm_bindgen`, ...).
    /// Configured in `.config/arch/thresholds.toml` (`[dead_exports]`).
    #[serde(default)]
    pub(crate) export_attrs: Vec<String>,
}

impl Default for DeadExportsThreshold {
    fn default() -> Self {
        Self {
            kinds: default_dead_exports_kinds(),
            test_crates: Vec::new(),
            ignore_crates: Vec::new(),
            exempt: Vec::new(),
            fix_protect_paths: Vec::new(),
            export_attrs: Vec::new(),
        }
    }
}

fn default_dead_exports_kinds() -> Vec<String> {
    [
        "fn", "method", "const", "static", "type", "struct", "enum", "trait",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenericParamCountThreshold {
    /// Items with this many generic params (type+lifetime+const) trigger a warn.
    pub(crate) warn_params: usize,
    /// Items with this many where-clause predicates trigger a warn.
    pub(crate) warn_where: usize,
}

impl Default for GenericParamCountThreshold {
    fn default() -> Self {
        Self {
            warn_params: 3,
            warn_where: 4,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TraitImplCountThreshold {
    /// Number of `impl Trait for X` blocks targeting one local type
    /// (per file, since the AST view is per-file). High count → god-type
    /// implementing too many roles.
    pub(crate) warn: usize,
}

impl Default for TraitImplCountThreshold {
    fn default() -> Self {
        Self { warn: 6 }
    }
}

fn default_shared_state_patterns() -> Vec<String> {
    vec![
        "Arc<Mutex<".to_string(),
        "Arc<RwLock<".to_string(),
        "Arc<parking_lot::Mutex<".to_string(),
        "Arc<parking_lot::RwLock<".to_string(),
        "Arc<tokio::sync::Mutex<".to_string(),
        "Arc<tokio::sync::RwLock<".to_string(),
    ]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlatDirectoryThreshold {
    pub(crate) warn: usize,
    pub(crate) deny: usize,
    #[serde(default)]
    pub(crate) ignore_globs: Vec<String>,
}

impl Default for FlatDirectoryThreshold {
    fn default() -> Self {
        Self {
            warn: 7,
            deny: 12,
            ignore_globs: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaxNestingThreshold {
    pub(crate) max_depth: usize,
    #[serde(default)]
    pub(crate) exempt_crates: Vec<String>,
}

impl Default for MaxNestingThreshold {
    fn default() -> Self {
        Self {
            max_depth: 2,
            exempt_crates: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadmePresenceThreshold {
    #[serde(default)]
    pub(crate) exempt: Vec<String>,
    pub(crate) min_bytes: u64,
}

impl Default for ReadmePresenceThreshold {
    fn default() -> Self {
        Self {
            exempt: Vec::new(),
            min_bytes: 200,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AccessorSeverity {
    Off,
    Warn,
    Deny,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SingleImplSizeThreshold {
    /// Soft threshold: warn at this many lines spanned by a single `impl`
    /// block (own or trait impl).
    pub(crate) warn_lines: usize,
    /// Hard threshold: deny at this many lines.
    pub(crate) deny_lines: usize,
}

impl Default for SingleImplSizeThreshold {
    fn default() -> Self {
        Self {
            warn_lines: 200,
            deny_lines: 400,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SingleWordFilenamesThreshold {
    /// Maximum number of `_`-separated tokens in the filename stem.
    /// Default 1 = filenames must be a single word (`peer.rs`, `source.rs`).
    /// `stream_type.rs` has 2 tokens; `a_b_c.rs` has 3.
    pub(crate) max_words: usize,
    /// Filenames that always pass regardless of word count
    /// (`mod.rs`, `lib.rs`, `main.rs`, `build.rs`).
    pub(crate) exempt_filenames: Vec<String>,
    /// Glob patterns of paths to skip entirely (e.g. `**/tests/**`).
    #[serde(default)]
    pub(crate) exempt_globs: Vec<String>,
    /// `warn` (track via baseline) or `deny` (fail on new offenders) or `off`.
    pub(crate) severity: AccessorSeverity,
}

impl Default for SingleWordFilenamesThreshold {
    fn default() -> Self {
        Self {
            max_words: 1,
            exempt_filenames: default_single_word_exempt_filenames(),
            exempt_globs: Vec::new(),
            severity: AccessorSeverity::Warn,
        }
    }
}

fn default_single_word_exempt_filenames() -> Vec<String> {
    ["mod.rs", "lib.rs", "main.rs", "build.rs"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedundantAccessorsThreshold {
    pub(crate) detect_field_passthrough: bool,
    pub(crate) detect_nested_shorthand: bool,
    pub(crate) detect_mutation_handle: bool,
    pub(crate) detect_delegate_passthrough: bool,
    pub(crate) p1_severity: AccessorSeverity,
    pub(crate) p2_severity: AccessorSeverity,
    pub(crate) p3_severity: AccessorSeverity,
    pub(crate) p4_severity: AccessorSeverity,
    pub(crate) public_only: bool,
    pub(crate) ignore_deref: bool,
    /// Types whose presence in a return type signals interior mutability
    /// (`AtomicU32`, `Mutex`, `RwLock`, ...). Detection recurses into generic
    /// arguments, so `Arc<AtomicUsize>` / `Option<&Mutex<T>>` are caught.
    #[serde(default = "default_mutable_handle_types")]
    pub(crate) mutable_handle_types: Vec<String>,
    /// Method names that mutate when applied to `self.<field>`: `store`, `set`,
    /// `swap`, `fetch_add`, ... Used by P3 to find a setter that targets the
    /// same field as a `*_handle()` getter.
    #[serde(default = "default_writer_methods")]
    pub(crate) writer_methods: Vec<String>,
    /// Single-arg constructor calls treated as transparent over their argument
    /// (`Some(&self.x)`, `Box::new(...)`, `Cow::Borrowed(...)`, `Arc::new(...)`,
    /// ...). Patterns are matched as plain (`Some`) or path suffix (`Box::new`).
    #[serde(default = "crate::common::parse::default_wrapper_ctors")]
    pub(crate) wrapper_ctors: Vec<String>,
    /// 0-arg methods that expose internal data (`as_ref`, `as_str`, `lock`,
    /// `borrow`, `read`, ...). The receiver chain is treated as the data path.
    /// `_mut` variants (`as_mut`, `borrow_mut`, `deref_mut`, `get_mut`,
    /// `write`) are emitted with `RefMut` access kind.
    #[serde(default = "crate::common::parse::default_expose_methods")]
    pub(crate) expose_methods: Vec<String>,
}

impl Default for RedundantAccessorsThreshold {
    fn default() -> Self {
        Self {
            detect_field_passthrough: true,
            detect_nested_shorthand: true,
            detect_mutation_handle: true,
            detect_delegate_passthrough: true,
            p1_severity: AccessorSeverity::Warn,
            p2_severity: AccessorSeverity::Warn,
            p3_severity: AccessorSeverity::Deny,
            p4_severity: AccessorSeverity::Warn,
            public_only: true,
            ignore_deref: true,
            mutable_handle_types: default_mutable_handle_types(),
            writer_methods: default_writer_methods(),
            wrapper_ctors: crate::common::parse::default_wrapper_ctors(),
            expose_methods: crate::common::parse::default_expose_methods(),
        }
    }
}

fn default_mutable_handle_types() -> Vec<String> {
    [
        "AtomicBool",
        "AtomicI8",
        "AtomicI16",
        "AtomicI32",
        "AtomicI64",
        "AtomicIsize",
        "AtomicU8",
        "AtomicU16",
        "AtomicU32",
        "AtomicU64",
        "AtomicUsize",
        "AtomicPtr",
        "Cell",
        "RefCell",
        "OnceCell",
        "LazyLock",
        "Mutex",
        "RwLock",
        "Notify",
        "Semaphore",
        "Condvar",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn default_writer_methods() -> Vec<String> {
    [
        "store",
        "swap",
        "fetch_add",
        "fetch_sub",
        "fetch_or",
        "fetch_and",
        "fetch_xor",
        "fetch_max",
        "fetch_min",
        "fetch_update",
        "fetch_nand",
        "compare_exchange",
        "compare_exchange_weak",
        "set",
        "replace",
        "replace_with",
        "take",
        "swap",
        "write",
        "send",
        "send_replace",
        "send_modify",
        "notify_one",
        "notify_all",
        "notify_waiters",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedEntitiesThreshold {
    /// A type is "sizable" if its `impl` surface has at least this many `fn`s
    /// across `impl X` and `impl Trait for X` blocks combined.
    pub(crate) min_fns_per_type: usize,
    /// A type also counts as "sizable" if it has at least this many `impl`
    /// blocks (own + trait impls). Catches structural types whose surface is
    /// spread across many trait implementations rather than methods.
    pub(crate) min_impl_blocks: usize,
    /// Files with this many sizable types → warn.
    pub(crate) warn: usize,
    /// Files with this many sizable types → deny.
    pub(crate) deny: usize,
}

impl Default for MixedEntitiesThreshold {
    fn default() -> Self {
        Self {
            min_fns_per_type: 5,
            min_impl_blocks: 3,
            warn: 2,
            deny: 3,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleLayersConfig {
    #[serde(default, rename = "crate")]
    pub(crate) crates: Vec<CrateLayers>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CrateLayers {
    pub(crate) name: String,
    #[serde(default, rename = "layer")]
    pub(crate) layers: Vec<ModuleLayer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModuleLayer {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) paths: Vec<String>,
}

fn load_optional<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("read config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse config: {}", path.display()))
}
