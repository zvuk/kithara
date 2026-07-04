use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_REL: &str = ".config/xtask.toml";

/// Project-specific identity and per-tool settings for the otherwise
/// project-agnostic xtask. Loaded from `.config/xtask.toml`; every field
/// defaults to empty so a fresh project starts with no baked-in names and
/// fills in only what it uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub project: ProjectIdentity,
    pub health: HealthConfig,
    pub test: TestCommandConfig,
    pub publish: PublishConfig,
    pub release: ReleaseConfig,
    pub lint_exclude: LintExcludeConfig,
    pub apple: AppleConfig,
    pub orphans: OrphansConfig,
    pub android: AndroidConfig,
    pub wasm: WasmConfig,
    pub quality: QualityConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrphansConfig {
    /// Packages excluded from the default `cargo modules orphans` sweep
    /// (generated/helper/macro crates and per-target-gated crates that the
    /// default rust-analyzer view flags as false-positive orphans).
    pub exclude_packages: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AndroidConfig {
    /// Cargo package compiled into the Android JNI libraries.
    pub ffi_crate: String,
    /// AAR artifacts the Gradle export is expected to produce.
    pub aars: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WasmConfig {
    /// wasm-bindgen JS artifact patched by the trunk post-build hook.
    pub js_artifact: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityConfig {
    /// Trait directory whose every `pub trait` must carry `#[unimock]`.
    pub unimock_traits_dir: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintExcludeConfig {
    /// Workspace-relative globs whose violations are dropped from every lint
    /// namespace (`arch`, `style`, `idioms`) so baselines measure production
    /// debt, not test code. `#[cfg(test)]` blocks are stripped automatically
    /// (AST) on top of this — no glob can match inline test modules.
    pub paths: Vec<String>,
    /// Inline-module names / `::`-paths whose violations are dropped from every
    /// lint namespace, regardless of file.
    pub modules: Vec<String>,
    /// ast-grep rule IDs that must scan the FULL tree — tests included —
    /// bypassing [`Self::paths`]. Hard-correctness bans (e.g. `arch.no-direct-time`)
    /// where test code is NOT exempt: routing time through one primitive only
    /// works if tests obey it too. Run in a second ast-grep pass per rule with
    /// no exclude globs; the rule's own `files:` / `ignores:` scope it.
    pub scan_all_rules: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectIdentity {
    /// Used in human-facing labels: health report title, temp-log prefix.
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Crates excluded from the `cargo hack --feature-powerset` stage.
    pub feature_powerset_exclude: Vec<String>,
    /// Crates excluded from whole-workspace stages (semver, nextest, doc-test).
    pub workspace_exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestCommandConfig {
    pub default_lane: String,
    pub default_backend: String,
    pub feature_arg: String,
    pub flash: TestFlashConfig,
    pub lanes: BTreeMap<String, TestLaneConfig>,
    pub net_backends: BTreeMap<String, TestNetBackendConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestFlashConfig {
    pub features: Vec<String>,
    pub default: bool,
}

impl Default for TestFlashConfig {
    fn default() -> Self {
        Self {
            features: Vec::new(),
            default: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestNetBackendConfig {
    pub features: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestLaneConfig {
    pub program: String,
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
    pub default_features: Vec<String>,
    pub default_flash: Option<bool>,
    pub passthrough: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReleaseConfig {
    /// GitHub repo (`owner/name`) that hosts the canonical releases.
    pub github_repo: String,
    /// Self-hosted `GitLab` instance that mirrors release artifacts.
    pub gitlab_host: String,
    /// `GitLab` project: numeric id or `group/name` path.
    pub gitlab_project: String,
    /// Generic package name in the `GitLab` registry.
    pub gitlab_package: String,
    /// Primary release asset: the SPM Rust `XCFramework` zip.
    pub asset: String,
    /// Optional single self-contained framework zip for manual drag-in. Empty
    /// disables that channel.
    pub single_asset: String,
    /// Documentation channel: zip name for the DocC archive uploaded as a
    /// release asset. Empty disables the docs channel.
    pub docs_asset: String,
    /// Workspace-relative DocC archive dir zipped into [`Self::docs_asset`]
    /// (the `just apple doc` output).
    pub docs_archive: String,
    /// WebAssembly channel: zip name for the trunk `dist` bundle deployed to
    /// GitHub Pages classic. Empty disables the wasm channel.
    pub wasm_asset: String,
    /// Workspace-relative trunk `dist` dir zipped into [`Self::wasm_asset`]
    /// (the `cargo xtask wasm build` output).
    pub wasm_dist: String,
    /// Branch GitHub Pages classic serves from (force-orphan deploy of the
    /// wasm bundle). Empty disables the pages deploy.
    pub pages_branch: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PublishConfig {
    /// Generated workspace-hack crate, stripped from published manifests.
    pub workspace_hack_crate: String,
    /// User-agent sent to the registry when checking crate availability.
    pub user_agent: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppleConfig {
    pub docgen: DocgenConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocgenConfig {
    /// Cargo package whose rustdoc JSON is the documentation source. The JSON
    /// filename stem is this name with dashes replaced by underscores.
    pub package: String,
    /// Features enabled for the rustdoc JSON build.
    pub features: Vec<String>,
    /// DocC module name used in the generated extension page headers.
    pub module: String,
    /// Workspace-relative directory the generated `.md` extensions are written
    /// to (a `.docc` catalog subfolder; gitignored, rebuilt by `just apple doc`).
    pub output_dir: String,
    /// facade DocC symbol → Rust type allowlist/mapping.
    pub symbols: Vec<DocgenSymbol>,
    /// Workspace-relative Swift source dirs whose every `public`/`open`
    /// declaration must carry a `///` doc comment. Enforced by
    /// `apple docgen --check` so no public symbol ships undocumented.
    pub swift_dirs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocgenSymbol {
    /// DocC symbol name in the facade module (e.g. `TrackStatus`).
    pub docc: String,
    /// Rust type name in the rustdoc JSON (e.g. `FfiTrackStatus`).
    pub rust: String,
}

impl ProjectConfig {
    /// Load project-specific xtask settings from `.config/xtask.toml`.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be read or parsed.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(CONFIG_REL);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read project config: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse project config: {}", path.display()))
    }
}
