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
pub(crate) struct ProjectConfig {
    pub(crate) project: ProjectIdentity,
    pub(crate) health: HealthConfig,
    pub(crate) test: TestCommandConfig,
    pub(crate) publish: PublishConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) lint_exclude: LintExcludeConfig,
    pub(crate) apple: AppleConfig,
    pub(crate) orphans: OrphansConfig,
    pub(crate) android: AndroidConfig,
    pub(crate) wasm: WasmConfig,
    pub(crate) quality: QualityConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrphansConfig {
    /// Packages excluded from the default `cargo modules orphans` sweep
    /// (generated/helper/macro crates and per-target-gated crates that the
    /// default rust-analyzer view flags as false-positive orphans).
    pub(crate) exclude_packages: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AndroidConfig {
    /// Cargo package compiled into the Android JNI libraries.
    pub(crate) ffi_crate: String,
    /// AAR artifacts the Gradle export is expected to produce.
    pub(crate) aars: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WasmConfig {
    /// wasm-bindgen JS artifact patched by the trunk post-build hook.
    pub(crate) js_artifact: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct QualityConfig {
    /// Trait directory whose every `pub trait` must carry `#[unimock]`.
    pub(crate) unimock_traits_dir: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LintExcludeConfig {
    /// Workspace-relative globs whose violations are dropped from every lint
    /// namespace (`arch`, `style`, `idioms`) so baselines measure production
    /// debt, not test code. `#[cfg(test)]` blocks are stripped automatically
    /// (AST) on top of this — no glob can match inline test modules.
    pub(crate) paths: Vec<String>,
    /// Inline-module names / `::`-paths whose violations are dropped from every
    /// lint namespace, regardless of file.
    pub(crate) modules: Vec<String>,
    /// ast-grep rule IDs that must scan the FULL tree — tests included —
    /// bypassing [`Self::paths`]. Hard-correctness bans (e.g. `arch.no-direct-time`)
    /// where test code is NOT exempt: routing time through one primitive only
    /// works if tests obey it too. Run in a second ast-grep pass per rule with
    /// no exclude globs; the rule's own `files:` / `ignores:` scope it.
    pub(crate) scan_all_rules: Vec<String>,
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
pub(crate) struct TestCommandConfig {
    pub(crate) default_lane: String,
    pub(crate) default_backend: String,
    pub(crate) feature_arg: String,
    pub(crate) flash: TestFlashConfig,
    pub(crate) lanes: BTreeMap<String, TestLaneConfig>,
    pub(crate) net_backends: BTreeMap<String, TestNetBackendConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TestFlashConfig {
    pub(crate) features: Vec<String>,
    pub(crate) default: bool,
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
pub(crate) struct TestNetBackendConfig {
    pub(crate) features: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TestLaneConfig {
    pub(crate) program: String,
    pub(crate) prefix_args: Vec<String>,
    pub(crate) suffix_args: Vec<String>,
    pub(crate) default_features: Vec<String>,
    pub(crate) default_flash: Option<bool>,
    pub(crate) passthrough: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReleaseConfig {
    /// GitHub repo (`owner/name`) that hosts the canonical releases.
    pub(crate) github_repo: String,
    /// Self-hosted `GitLab` instance that mirrors release artifacts.
    pub(crate) gitlab_host: String,
    /// `GitLab` project: numeric id or `group/name` path.
    pub(crate) gitlab_project: String,
    /// Generic package name in the `GitLab` registry.
    pub(crate) gitlab_package: String,
    /// Primary release asset: the SPM Rust `XCFramework` zip.
    pub(crate) asset: String,
    /// Optional single self-contained framework zip for manual drag-in. Empty
    /// disables that channel.
    pub(crate) single_asset: String,
    /// Documentation channel: zip name for the DocC archive uploaded as a
    /// release asset. Empty disables the docs channel.
    pub(crate) docs_asset: String,
    /// Workspace-relative DocC archive dir zipped into [`Self::docs_asset`]
    /// (the `just apple doc` output).
    pub(crate) docs_archive: String,
    /// WebAssembly channel: zip name for the trunk `dist` bundle deployed to
    /// GitHub Pages classic. Empty disables the wasm channel.
    pub(crate) wasm_asset: String,
    /// Workspace-relative trunk `dist` dir zipped into [`Self::wasm_asset`]
    /// (the `cargo xtask wasm build` output).
    pub(crate) wasm_dist: String,
    /// Branch GitHub Pages classic serves from (force-orphan deploy of the
    /// wasm bundle). Empty disables the pages deploy.
    pub(crate) pages_branch: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PublishConfig {
    /// Generated workspace-hack crate, stripped from published manifests.
    pub(crate) workspace_hack_crate: String,
    /// User-agent sent to the registry when checking crate availability.
    pub(crate) user_agent: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AppleConfig {
    pub(crate) docgen: DocgenConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DocgenConfig {
    /// Cargo package whose rustdoc JSON is the documentation source. The JSON
    /// filename stem is this name with dashes replaced by underscores.
    pub(crate) package: String,
    /// Features enabled for the rustdoc JSON build.
    pub(crate) features: Vec<String>,
    /// DocC module name used in the generated extension page headers.
    pub(crate) module: String,
    /// Workspace-relative directory the generated `.md` extensions are written
    /// to (a `.docc` catalog subfolder; gitignored, rebuilt by `just apple doc`).
    pub(crate) output_dir: String,
    /// facade DocC symbol → Rust type allowlist/mapping.
    pub(crate) symbols: Vec<DocgenSymbol>,
    /// Workspace-relative Swift source dirs whose every `public`/`open`
    /// declaration must carry a `///` doc comment. Enforced by
    /// `apple docgen --check` so no public symbol ships undocumented.
    pub(crate) swift_dirs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocgenSymbol {
    /// DocC symbol name in the facade module (e.g. `TrackStatus`).
    pub(crate) docc: String,
    /// Rust type name in the rustdoc JSON (e.g. `FfiTrackStatus`).
    pub(crate) rust: String,
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
