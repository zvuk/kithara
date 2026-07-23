use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::project::ProjectConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct KitharaExt {
    pub(crate) android: AndroidConfig,
    pub(crate) wasm: WasmConfig,
    pub(crate) apple: AppleConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) publish: PublishConfig,
    xtask: Option<XtaskConfig>,
    agent_hook: Option<AgentHookConfig>,
}

impl KitharaExt {
    pub(crate) fn from_ctx(ctx: &Ctx) -> Result<Self> {
        Self::from_project_config(&ctx.config)
    }

    pub(crate) fn load(root: &Path) -> Result<Self> {
        let config = ProjectConfig::load(root)?;
        Self::from_project_config(&config)
    }

    fn from_project_config(config: &ProjectConfig) -> Result<Self> {
        toml::Value::Table(config.ext.clone())
            .try_into()
            .context("parse project config [ext]")
    }

    pub(crate) fn xtask_cache(&self) -> Result<&XtaskCacheConfig> {
        let config = self
            .xtask
            .as_ref()
            .and_then(|xtask| xtask.cache.as_ref())
            .context("ext.xtask.cache is not set in .config/xtask.toml")?;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn agent_hook(&self) -> Result<&AgentHookConfig> {
        let config = self
            .agent_hook
            .as_ref()
            .context("ext.agent_hook is not set in .config/xtask.toml")?;
        config.validate()?;
        Ok(config)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct XtaskConfig {
    cache: Option<XtaskCacheConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct XtaskCacheConfig {
    pub(crate) extra_inputs: Vec<PathBuf>,
    pub(crate) keep_generations: usize,
    pub(crate) generation_grace_secs: u64,
}

impl XtaskCacheConfig {
    fn validate(&self) -> Result<()> {
        if self.keep_generations < 2 {
            bail!("ext.xtask.cache.keep_generations must be at least 2");
        }
        if self.generation_grace_secs == 0 {
            bail!("ext.xtask.cache.generation_grace_secs must be positive");
        }
        for path in &self.extra_inputs {
            if path.as_os_str().is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
            {
                bail!(
                    "ext.xtask.cache.extra_inputs must contain project-relative paths: {}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentHookConfig {
    pub(crate) destructive_git_override_env: String,
    pub(crate) routes: Vec<HookRoute>,
}

impl AgentHookConfig {
    fn validate(&self) -> Result<()> {
        if self.destructive_git_override_env.is_empty() {
            bail!("ext.agent_hook.destructive_git_override_env must not be empty");
        }
        if self.routes.is_empty() {
            bail!("ext.agent_hook.routes must not be empty");
        }
        let mut routes = BTreeSet::new();
        for route in &self.routes {
            let compatible = matches!(
                (route.event, route.tool_kind, route.handler),
                (
                    HookEvent::PreToolUse,
                    HookToolKind::Shell,
                    HookHandler::CommandGuard
                ) | (
                    HookEvent::PostToolUse,
                    HookToolKind::FileEdit,
                    HookHandler::FormatEditedPaths
                )
            );
            if !compatible {
                bail!(
                    "ext.agent_hook route {:?}/{:?} is incompatible with handler {:?}",
                    route.event,
                    route.tool_kind,
                    route.handler
                );
            }
            if !routes.insert((route.event, route.tool_kind)) {
                bail!(
                    "ext.agent_hook.routes contains a duplicate {:?}/{:?} route",
                    route.event,
                    route.tool_kind
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookEvent {
    PreToolUse,
    PostToolUse,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookToolKind {
    Shell,
    FileEdit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookHandler {
    CommandGuard,
    FormatEditedPaths,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookRoute {
    pub(crate) event: HookEvent,
    pub(crate) tool_kind: HookToolKind,
    pub(crate) handler: HookHandler,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AndroidConfig {
    /// Cargo package compiled into the Android JNI libraries.
    pub(crate) ffi_crate: String,
    /// AAR artifacts the Gradle export is expected to produce.
    pub(crate) aars: Vec<String>,
    /// AVD name used by `android run` when `--avd` is omitted.
    pub(crate) default_avd: String,
    /// Android demo application id installed and launched by `android run`.
    pub(crate) demo_package: String,
    /// Android demo activity component launched by `android run`.
    pub(crate) demo_activity: String,
    /// Android API level passed to `cargo ndk`.
    pub(crate) api_level: String,
    /// Number of boot-completion polls before `android run` gives up.
    pub(crate) boot_wait_attempts: Option<u32>,
    /// Seconds between Android boot-completion polls.
    pub(crate) boot_poll_interval_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WasmConfig {
    /// wasm-bindgen JS artifact patched by the trunk post-build hook.
    pub(crate) js_artifact: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReleaseConfig {
    /// Swift package manifest stamped and read during release prepare/publish.
    pub(crate) manifest: String,
    /// Product name used in generated release titles.
    pub(crate) title: String,
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
    /// Seconds before `GitLab` API curl requests time out.
    pub(crate) http_timeout_secs: Option<u64>,
    /// Seconds before `GitLab` package upload curl requests time out.
    pub(crate) upload_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PublishConfig {
    /// Generated workspace-hack crate, stripped from published manifests.
    pub(crate) workspace_hack_crate: String,
    /// Delay in seconds between crate uploads when `--delay` is omitted.
    pub(crate) delay_secs: Option<u64>,
    /// Seconds before crates.io availability checks time out.
    pub(crate) http_timeout_secs: Option<u64>,
    /// User-agent sent to the registry when checking crate availability.
    pub(crate) user_agent: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AppleConfig {
    /// Simulator name used by `apple run` when `--simulator` is omitted.
    pub(crate) default_simulator: String,
    /// Xcode scheme used by `apple run` when `--scheme` is omitted.
    pub(crate) default_scheme: String,
    /// Bundle id launched by `apple run`.
    pub(crate) demo_bundle_id: String,
    /// Symbol substrings forbidden in Apple release `XCFramework` slices.
    pub(crate) banned_symbol_needles: Vec<String>,
    /// Symbol substrings proving the Apple backend is linked in every slice.
    pub(crate) apple_proof_needles: Vec<String>,
    /// DocC documentation-extension generator configuration.
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
    /// facade DocC symbol -> Rust type allowlist/mapping.
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kithara_devtools::Ctx;

    use super::KitharaExt;

    fn ctx_from_config(text: &str) -> Ctx {
        Ctx {
            root: PathBuf::new(),
            config: toml::from_str(text).expect("parse project config"),
        }
    }

    #[test]
    fn unknown_ext_sibling_sections_are_passthrough() {
        let ctx = ctx_from_config(
            r#"
[ext.android]
ffi_crate = "kithara-ffi"

[ext.local_tool]
enabled = true
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        assert_eq!(ext.android.ffi_crate, "kithara-ffi");
    }

    #[test]
    fn known_ext_sections_reject_unknown_fields() {
        let ctx = ctx_from_config(
            r#"
[ext.android]
ffi_crate = "kithara-ffi"
typo = true
"#,
        );

        let error = KitharaExt::from_ctx(&ctx).expect_err("android typo fails");
        let message = format!("{error:#}");

        assert!(
            message.contains("typo"),
            "error did not mention offending token: {message}"
        );
    }

    #[test]
    fn migrated_xtask_ext_fields_parse() {
        let ctx = ctx_from_config(
            r#"
[ext.publish]
workspace_hack_crate = "kithara-workspace-hack"
delay_secs = 20
http_timeout_secs = 20
user_agent = "kithara-xtask-publish"

[ext.release]
manifest = "Package.swift"
title = "Kithara"
github_repo = "zvuk/kithara"
gitlab_host = "gitlab.zvq.me"
gitlab_project = "disrupt/kithara"
gitlab_package = "kithara"
asset = "KitharaFFIInternal.xcframework.zip"
http_timeout_secs = 60
upload_timeout_secs = 600

[ext.android]
ffi_crate = "kithara-ffi"
aars = ["kithara.aar"]
default_avd = "Pixel_6"
demo_package = "com.kithara.example"
demo_activity = "com.kithara.example.MainActivity"
api_level = "26"
boot_wait_attempts = 120
boot_poll_interval_secs = 1

[ext.apple]
default_simulator = "iPhone 17 Pro Max"
default_scheme = "KitharaDemo_iOS"
demo_bundle_id = "com.kithara.demo"
banned_symbol_needles = ["symphonia_bundle_"]
apple_proof_needles = ["AppleCodec"]
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        assert_eq!(ext.publish.delay_secs, Some(20));
        assert_eq!(ext.publish.http_timeout_secs, Some(20));
        assert_eq!(ext.release.manifest, "Package.swift");
        assert_eq!(ext.release.title, "Kithara");
        assert_eq!(ext.release.http_timeout_secs, Some(60));
        assert_eq!(ext.release.upload_timeout_secs, Some(600));
        assert_eq!(ext.android.default_avd, "Pixel_6");
        assert_eq!(ext.android.boot_wait_attempts, Some(120));
        assert_eq!(ext.android.boot_poll_interval_secs, Some(1));
        assert_eq!(ext.apple.default_simulator, "iPhone 17 Pro Max");
        assert_eq!(ext.apple.default_scheme, "KitharaDemo_iOS");
        assert_eq!(ext.apple.demo_bundle_id, "com.kithara.demo");
        assert_eq!(ext.apple.banned_symbol_needles, ["symphonia_bundle_"]);
        assert_eq!(ext.apple.apple_proof_needles, ["AppleCodec"]);
    }

    #[test]
    fn migrated_xtask_ext_fields_reject_unknown_fields() {
        let ctx = ctx_from_config(
            r#"
[ext.apple]
default_simulator = "iPhone 17 Pro Max"
default_scheme = "KitharaDemo_iOS"
demo_bundle_id = "com.kithara.demo"
banned_symbol_needles = ["symphonia_bundle_"]
apple_proof_needles = ["AppleCodec"]
typo = true
"#,
        );

        let error = KitharaExt::from_ctx(&ctx).expect_err("apple typo fails");
        let message = format!("{error:#}");

        assert!(
            message.contains("typo"),
            "error did not mention offending token: {message}"
        );
    }

    #[test]
    fn self_cache_and_hook_sections_are_required_and_typed() {
        let ctx = ctx_from_config(
            r#"
[ext.xtask.cache]
extra_inputs = ["justfile"]
keep_generations = 2
generation_grace_secs = 3600

[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");
        let cache = ext.xtask_cache().expect("resolve xtask cache config");
        let hook = ext.agent_hook().expect("resolve agent hook config");

        assert_eq!(cache.keep_generations, 2);
        assert_eq!(cache.generation_grace_secs, 3600);
        assert_eq!(cache.extra_inputs, [PathBuf::from("justfile")]);
        assert_eq!(hook.routes.len(), 1);
    }

    #[test]
    fn missing_self_cache_and_hook_sections_fail_resolution() {
        let ctx = ctx_from_config("");
        let ext = KitharaExt::from_ctx(&ctx).expect("parse empty extension");

        assert!(ext.xtask_cache().is_err());
        assert!(ext.agent_hook().is_err());
    }

    #[test]
    fn hook_routes_reject_incompatible_handler_types() {
        let ctx = ctx_from_config(
            r#"
[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "file-edit"
handler = "command-guard"
"#,
        );
        let ext = KitharaExt::from_ctx(&ctx).expect("parse hook extension");

        let error = ext
            .agent_hook()
            .expect_err("incompatible hook handler must fail");

        assert!(format!("{error:#}").contains("incompatible"));
    }

    #[test]
    fn owned_self_cache_section_rejects_unknown_fields() {
        let ctx = ctx_from_config(
            r#"
[ext.xtask.cache]
extra_inputs = []
keep_generations = 2
generation_grace_secs = 3600
typo = true
"#,
        );

        let error = KitharaExt::from_ctx(&ctx).expect_err("cache typo fails");

        assert!(format!("{error:#}").contains("typo"));
    }
}
