//! Loaders for `.config/arch/*.toml`.
//!
//! Each check has its own typed config section. Missing files default to empty,
//! which lets checks degrade gracefully when only some configs are present.

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

// --- direction.toml --------------------------------------------------------

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

// --- canonical-types.toml --------------------------------------------------

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

// --- thresholds.toml -------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThresholdsConfig {
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
            exempt_filenames: vec![
                "mod.rs".into(),
                "lib.rs".into(),
                "main.rs".into(),
                "build.rs".into(),
            ],
            exempt_globs: Vec::new(),
            severity: AccessorSeverity::Warn,
        }
    }
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
        // Atomic*
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
        // Cell / RefCell / OnceCell
        "set",
        "replace",
        "replace_with",
        "take",
        "swap",
        // Mutex / RwLock writers (callers do *guard = X manually so plain assignment also caught)
        "write",
        "send",
        "send_replace",
        "send_modify",
        // Notify
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

// --- module-layers.toml ----------------------------------------------------

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

// --- helpers ---------------------------------------------------------------

fn load_optional<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("read config: {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse config: {}", path.display()))
}
