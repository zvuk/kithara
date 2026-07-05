use anyhow::{Context, Result};
use kithara_devtools::Ctx;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct KitharaExt {
    pub(crate) android: AndroidConfig,
    pub(crate) wasm: WasmConfig,
    pub(crate) apple: AppleConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) publish: PublishConfig,
}

impl KitharaExt {
    pub(crate) fn from_ctx(ctx: &Ctx) -> Result<Self> {
        toml::Value::Table(ctx.config.ext.clone())
            .try_into()
            .context("parse project config [ext]")
    }
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
}
