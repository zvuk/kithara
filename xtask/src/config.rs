use std::{
    collections::{BTreeMap, BTreeSet},
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
    pub(crate) ci: CiProjectConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) publish: PublishConfig,
    agent_hook: Option<AgentHookConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiProjectConfig {
    pub(crate) pins: PathBuf,
    pub(crate) lanes: BTreeMap<String, CiLaneConfig>,
    pub(crate) verdict: CiVerdictConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiVerdictConfig {
    /// Old test ID prefixes mapped to their current names. Test source moves
    /// change nextest's package and binary prefix without changing the test,
    /// while the executor's journal necessarily still contains the old ID.
    pub(crate) id_aliases: BTreeMap<String, String>,
}

/// A CI lane that is nothing but the work it asks the executor for. Lanes that
/// need more than parameters keep a function; this is what the rest are.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiLaneConfig {
    /// The shared cache the lane leases, named the way the runner tags are.
    pub(crate) cache_group: String,
    /// How the lane names itself when it refuses a platform.
    pub(crate) label: String,
    pub(crate) os: Option<String>,
    pub(crate) tools: Vec<String>,
    /// Tools whose reported version has to match a reviewed pin before the lane
    /// spends a runner on a build it would have to throw away.
    pub(crate) pinned: Vec<CiLanePin>,
    /// Paths a predecessor job has to have left in the checkout, and the job
    /// that leaves them. A lane that arrives without them fails minutes in, on
    /// a device, with nothing more informative than a link error.
    pub(crate) left_behind: Vec<String>,
    pub(crate) left_behind_by: String,
    pub(crate) program: String,
    pub(crate) steps: Vec<CiLaneStep>,
    /// Pipeline kinds this lane refuses rather than runs. A lane that is not
    /// scheduled in a pipeline never reaches this; one that is scheduled and
    /// declines has to say so where the schedule can be read against it.
    pub(crate) kinds_refused: BTreeMap<String, String>,
    /// Which role workflow schedules this lane. Roles are a field rather than
    /// a workflow each, because five workflows differing by one string is the
    /// duplication this catalog exists to remove.
    pub(crate) role: String,
    /// Pipeline kinds this lane runs in. Empty means the lane is reachable
    /// only by name, through a dispatch that asks for it.
    pub(crate) kinds: Vec<String>,
    /// The GitHub fleet's answer where it honestly differs from `kinds`: 25
    /// runners on one host buy a check per push that a single Mac mini can
    /// only afford weekly. Empty means both fleets agree.
    pub(crate) kinds_github: Vec<String>,
    /// A stable GitHub runner label for lanes whose persistent build cache
    /// must stay on one runner slot. Empty keeps the lane on the shared pool.
    pub(crate) github_runner: Option<String>,
    pub(crate) timeout_minutes: u32,
    /// Checkout depth. Zero is full history, which a lane comparing against a
    /// base revision needs and a shallow clone does not carry.
    pub(crate) fetch_depth: u32,
    pub(crate) artifact: Option<CiLaneArtifact>,
    /// The concurrency group a lane wanting the whole host queues in.
    pub(crate) queue: Option<String>,
    /// Lanes whose artifacts this one consumes. A lane with needs runs after
    /// them and only when at least one of them was selected.
    pub(crate) needs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiLaneStep {
    pub(crate) args: Vec<String>,
    pub(crate) label: String,
    /// What the step needs the executor to be, rather than to run: a build-job
    /// cap the container cannot exceed, a target directory a gate owns, the
    /// browser a harness would otherwise guess. A value may name the checkout
    /// with `{root}`, or the leased build-cache directory with `{target}` -
    /// the two things a lane cannot spell for itself.
    pub(crate) env: BTreeMap<String, String>,
    /// The program for this step alone. A lane that installs a target before
    /// using it runs two, so the lane's own `program` is only the default.
    pub(crate) program: Option<String>,
    /// Arguments for one pipeline kind, replacing `args` there. A review ref
    /// and the default branch ask the same question of a gate; a quarantine
    /// run deliberately asks a narrower one.
    pub(crate) args_by_kind: BTreeMap<String, Vec<String>>,
}

/// What a lane leaves for a human or a later lane to read.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiLaneArtifact {
    pub(crate) name: String,
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) when: ArtifactWhen,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ArtifactWhen {
    #[default]
    Always,
    Failure,
}

/// The pipeline kinds a lane may name. Kept beside the cache groups, which the
/// catalog already validates by name for the same reason: the executor's enum
/// lives in `ci::run`, and a lane is refused at configuration load, before any
/// executor is consulted. `lane_config.rs` pins the two lists together.
pub(crate) const PIPELINE_KINDS: [&str; 8] = [
    "branch",
    "platforms",
    "merge-request",
    "quarantine",
    "main",
    "nightly",
    "weekly",
    "release",
];

pub(crate) const LANE_ROLES: [&str; 5] = ["gate", "platforms", "deep", "quality", "release"];

/// The lane's own executable. A Windows job runs the binary it started as
/// rather than `cargo xtask`, which would rebuild it - and Windows refuses to
/// replace a running image, so Cargo reported that as a failure to remove
/// `xtask.exe`.
pub(crate) const SELF_PROGRAM: &str = "<xtask>";

/// The checkout a lane resolves in. A compiler flag that has to name a file in
/// the repository needs an absolute path, and only the runner knows it.
pub(crate) const ROOT_PLACEHOLDER: &str = "{root}";

/// The build-cache directory the process leased, i.e. `CARGO_TARGET_DIR` as
/// the executor set it, not `{root}/target`. A lane that writes its own build
/// under a fixed `{root}`-relative path escapes the lease the eviction and
/// reclaim machinery tracks; one that asks for `{target}` stays inside it the
/// same way the process's own build does.
pub(crate) const TARGET_PLACEHOLDER: &str = "{target}";

/// A reviewed pin, by name: `{pin.msrv_toolchain}` is the value that key holds
/// in `.config/ci-pins.toml`.
pub(crate) const PIN_PREFIX: &str = "{pin.";

/// A version check: ask `tool` how old it is, and require the answer to carry
/// the value `pin` names in `.config/ci-pins.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiLanePin {
    pub(crate) tool: String,
    pub(crate) args: Vec<String>,
    pub(crate) pin: String,
    /// What the reported version has to read in full, on its first line, for
    /// tools that print more than a number. Without it any whitespace-separated
    /// word matching the pin is accepted.
    pub(crate) line_prefix: Option<String>,
}

impl CiProjectConfig {
    fn validate_lanes(&self) -> Result<()> {
        for (name, lane) in &self.lanes {
            if !matches!(
                lane.cache_group.as_str(),
                "macos" | "linux" | "windows" | "host"
            ) {
                bail!(
                    "ext.ci.lanes.{name}.cache_group must be macos, linux, windows or host, got `{}`",
                    lane.cache_group
                );
            }
            if lane.program.is_empty() {
                bail!("ext.ci.lanes.{name} must name a program");
            }
            if lane.steps.is_empty() {
                bail!("ext.ci.lanes.{name} must declare at least one step");
            }
            // Every lane names the machine it needs. The GitHub fan-out has one
            // runner pool and it is Linux, so a lane that named none would be
            // scheduled onto it by omission rather than by declaration, and run
            // an emulator recipe with no emulator under it.
            let Some(os) = lane.os.as_deref() else {
                bail!("ext.ci.lanes.{name} must name the operating system it runs on");
            };
            if !matches!(os, "linux" | "macos" | "windows") {
                bail!("ext.ci.lanes.{name}.os must be linux, macos or windows, got `{os}`");
            }
            // `kinds_github` is a statement that GitHub schedules this lane, and
            // GitHub's fan-out reaches one pool. A lane naming another machine
            // would be refused at selection and never run, which is a lane
            // declared into a schedule it cannot reach - the failure this
            // catalog exists to make impossible, not one to restate quietly.
            if !lane.kinds_github.is_empty() && os != "linux" {
                bail!(
                    "ext.ci.lanes.{name}.kinds_github schedules a `{os}` lane, and the GitHub fleet is Linux"
                );
            }
            if lane.github_runner.as_deref().is_some_and(str::is_empty) {
                bail!("ext.ci.lanes.{name}.github_runner must not be empty");
            }
            if lane.label.is_empty() {
                bail!("ext.ci.lanes.{name} must carry a label to refuse under");
            }
            for check in &lane.pinned {
                if check.tool.is_empty() || check.pin.is_empty() {
                    bail!("ext.ci.lanes.{name}.pinned must name both a tool and a pin");
                }
            }
            if !lane.left_behind.is_empty() && lane.left_behind_by.is_empty() {
                bail!("ext.ci.lanes.{name} must name the job its left_behind paths come from");
            }
            for step in &lane.steps {
                for (key, value) in &step.env {
                    validate_substitutions(name, key, value)?;
                }
                let by_kind = step.args_by_kind.values().flatten();
                for value in step.args.iter().chain(by_kind) {
                    validate_substitutions(name, "an argument", value)?;
                }
            }
            if !LANE_ROLES.contains(&lane.role.as_str()) {
                bail!(
                    "ext.ci.lanes.{name}.role must be one of {}, got `{}`",
                    LANE_ROLES.join(", "),
                    lane.role
                );
            }
            for (field, listed) in [("kinds", &lane.kinds), ("kinds_github", &lane.kinds_github)] {
                for kind in listed {
                    if !PIPELINE_KINDS.contains(&kind.as_str()) {
                        bail!("ext.ci.lanes.{name}.{field} names unknown kind `{kind}`");
                    }
                }
            }
            if lane.timeout_minutes == 0 {
                bail!("ext.ci.lanes.{name} must declare a non-zero timeout_minutes");
            }
        }
        for (name, lane) in &self.lanes {
            for needed in &lane.needs {
                if !self.lanes.contains_key(needed) {
                    bail!("ext.ci.lanes.{name}.needs names `{needed}`, which is not a lane");
                }
                if needed == name {
                    bail!("ext.ci.lanes.{name} cannot need itself");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_lanes()?;
        for (old, new) in &self.verdict.id_aliases {
            if old.is_empty() || new.is_empty() || old == new {
                bail!(
                    "ext.ci.verdict.id_aliases must map a non-empty old prefix to a different prefix"
                );
            }
        }
        if self.pins.as_os_str().is_empty()
            || self.pins.is_absolute()
            || self
                .pins
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!("ext.ci.pins must be a project-relative file");
        }
        Ok(())
    }
}

/// `{root}`, `{target}`, and `{pin.<key>}` are the whole substitution
/// vocabulary. A typo that reached the runner would be passed through as a
/// literal brace and fail as a missing header or an unknown toolchain rather
/// than as a bad config.
fn validate_substitutions(lane: &str, whose: &str, value: &str) -> Result<()> {
    let mut rest = value
        .replace(ROOT_PLACEHOLDER, "")
        .replace(TARGET_PLACEHOLDER, "");
    while let Some(start) = rest.find(PIN_PREFIX) {
        let Some(end) = rest[start..].find('}') else {
            bail!("ext.ci.lanes.{lane} leaves {PIN_PREFIX} unclosed in {whose}: `{value}`");
        };
        rest.replace_range(start..=start + end, "");
    }
    if rest.contains('{') {
        bail!(
            "ext.ci.lanes.{lane} names something other than {ROOT_PLACEHOLDER}, \
             {TARGET_PLACEHOLDER}, or {PIN_PREFIX}<key>}} in {whose}: `{value}`"
        );
    }
    Ok(())
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

/// The self-cache view of `.config/xtask.toml`: `ext.xtask.cache` and nothing
/// else. A cached binary must stay able to report its own freshness across a
/// schema change in a section it does not own, or the generation that predates
/// the change can never be refreshed.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheDocument {
    ext: CacheExt,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheExt {
    xtask: XtaskConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct XtaskCacheConfig {
    pub(crate) extra_inputs: Vec<PathBuf>,
    pub(crate) keep_generations: usize,
    pub(crate) generation_grace_secs: u64,
}

impl XtaskCacheConfig {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let path = root.join(".config/xtask.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read project config {}", path.display()))?;
        let document: CacheDocument = toml::from_str(&text)
            .with_context(|| format!("parse project config {}", path.display()))?;
        let config = document
            .ext
            .xtask
            .cache
            .context("ext.xtask.cache is not set in .config/xtask.toml")?;
        config.validate()?;
        Ok(config)
    }

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
    /// Tag the rolling build channel replaces on every nightly run. Empty
    /// disables that channel.
    pub(crate) nightly_tag: String,
    /// Rust core plus its `UniFFI` binding, consumed as the Swift package's
    /// binary target.
    pub(crate) core_asset: String,
    /// Swift layer merged into the framework for manual drag-in consumers.
    /// Empty disables that channel.
    pub(crate) merged_asset: String,
    /// Additional required CI-built artifacts published with the Apple
    /// frameworks, such as Android AARs.
    pub(crate) platform_assets: Vec<String>,
    /// Documentation channel: zip name for the DocC archive uploaded as a
    /// release asset. Empty disables the docs channel.
    pub(crate) docs_asset: String,
    /// Workspace-relative DocC archive dir zipped into [`Self::docs_asset`]
    /// (the `just platform apple doc` output).
    pub(crate) docs_archive: String,
    /// WebAssembly channel: zip name for the trunk `dist` bundle deployed to
    /// GitHub Pages classic. Empty disables the wasm channel.
    pub(crate) wasm_asset: String,
    /// Workspace-relative trunk `dist` dir zipped into [`Self::wasm_asset`]
    /// (the `just platform wasm build` output).
    pub(crate) wasm_dist: String,
    /// Branch GitHub Pages classic serves from (force-orphan deploy of the
    /// wasm bundle). Empty disables the pages deploy.
    pub(crate) pages_branch: String,
    /// Seconds before `GitLab` API curl requests time out.
    pub(crate) http_timeout_secs: Option<u64>,
    /// Seconds before `GitLab` package upload curl requests time out.
    pub(crate) upload_timeout_secs: Option<u64>,
    /// Named packaging profiles. A lane names one; nothing infers it.
    pub(crate) packages: BTreeMap<String, PackageProfile>,
    /// Named delivery channels. A lane names one; nothing infers it.
    pub(crate) channels: BTreeMap<String, ChannelProfile>,
}

impl ReleaseConfig {
    pub(crate) fn package(&self, name: &str) -> Result<&PackageProfile> {
        self.packages
            .get(name)
            .with_context(|| format!("ext.release.packages.{name} is not defined"))
    }

    pub(crate) fn channel(&self, name: &str) -> Result<&ChannelProfile> {
        self.channels
            .get(name)
            .with_context(|| format!("ext.release.channels.{name} is not defined"))
    }

    pub(crate) fn asset_name(&self, key: AssetKey) -> &str {
        match key {
            AssetKey::Core => &self.core_asset,
            AssetKey::Merged => &self.merged_asset,
            AssetKey::Docs => &self.docs_asset,
            AssetKey::Wasm => &self.wasm_asset,
        }
    }
}

/// One packaged artifact, named by what it carries rather than by file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssetKey {
    Core,
    Merged,
    Docs,
    Wasm,
}

/// What a packaging run collects. Whether the built framework has to match a
/// version belongs to the pipeline rather than to the profile: one job builds
/// the same assets for a release someone named a version for and for the
/// rolling nightly, which names none.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PackageProfile {
    pub(crate) assets: Vec<AssetKey>,
}

/// One step of delivery. Naming the steps individually is what lets a channel
/// be data the config carries rather than a branch the code takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PublishStep {
    Retained,
    NightlyRetained,
    Pages,
    Crates,
}

/// What a delivery channel requires before it runs and what it then performs.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ChannelProfile {
    /// Whether `KITHARA_RELEASE_VERSION` must be set and agree with the
    /// manifest at the published commit.
    pub(crate) requires_version: bool,
    /// Whether every retained asset must be present, or only those that are.
    pub(crate) require_all_assets: bool,
    pub(crate) tokens: Vec<String>,
    pub(crate) steps: Vec<PublishStep>,
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
    /// to (a `.docc` catalog subfolder; gitignored, rebuilt by
    /// `just platform apple doc`).
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
    use tempfile::TempDir;

    use super::{AssetKey, KitharaExt, PublishStep, XtaskCacheConfig};

    fn config_root(body: &str) -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("create fixture root");
        let root = temp.path().to_path_buf();
        std::fs::create_dir_all(root.join(".config")).expect("create config dir");
        std::fs::write(root.join(".config/xtask.toml"), body).expect("write project config");
        (temp, root)
    }

    fn ctx_from_config(text: &str) -> Ctx {
        Ctx::new(
            PathBuf::new(),
            toml::from_str(text).expect("parse project config"),
        )
    }

    #[test]
    fn release_assets_are_named_by_layer() {
        let ctx = ctx_from_config(
            r#"
[ext.release]
core_asset = "KitharaFFIInternal.xcframework.zip"
merged_asset = "Kithara.xcframework.zip"
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        assert_eq!(ext.release.core_asset, "KitharaFFIInternal.xcframework.zip");
        assert_eq!(ext.release.merged_asset, "Kithara.xcframework.zip");
    }

    #[test]
    fn a_packaging_profile_names_its_assets() {
        let ctx = ctx_from_config(
            r#"
[ext.release]
core_asset = "KitharaFFIInternal.xcframework.zip"
merged_asset = "Kithara.xcframework.zip"

[ext.release.packages.snapshot]
assets = ["merged"]
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        let profile = ext.release.package("snapshot").expect("snapshot profile");

        assert_eq!(profile.assets, vec![AssetKey::Merged]);
    }

    #[test]
    fn the_release_channel_resolves_to_todays_hard_coded_behaviour() {
        let ctx = ctx_from_config(
            r#"
[ext.release.channels.release]
requires_version = true
require_all_assets = true
tokens = ["CARGO_REGISTRY_TOKEN", "GH_TOKEN", "GITLAB_TOKEN"]
steps = ["retained", "pages", "crates"]

[ext.release.channels.nightly]
requires_version = false
require_all_assets = false
tokens = ["GH_TOKEN", "GITLAB_TOKEN"]
steps = ["nightly_retained"]
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        let release = ext.release.channel("release").expect("release channel");
        assert!(release.requires_version);
        assert!(release.require_all_assets);
        assert_eq!(
            release.tokens,
            ["CARGO_REGISTRY_TOKEN", "GH_TOKEN", "GITLAB_TOKEN"]
        );
        assert_eq!(
            release.steps,
            vec![
                PublishStep::Retained,
                PublishStep::Pages,
                PublishStep::Crates
            ]
        );

        let nightly = ext.release.channel("nightly").expect("nightly channel");
        assert!(!nightly.requires_version);
        assert!(!nightly.require_all_assets);
        assert_eq!(nightly.tokens, ["GH_TOKEN", "GITLAB_TOKEN"]);
        assert_eq!(nightly.steps, vec![PublishStep::NightlyRetained]);
    }

    // A lane that names a machine and then declares GitHub schedules it is two
    // statements that cannot both be true. Selection refuses the lane silently,
    // so the catalog says the lane runs nightly and nothing ever runs it.
    #[test]
    fn a_lane_off_the_github_fleet_may_not_declare_a_github_schedule() {
        let ctx = ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.apple-thing]
cache_group = "macos"
label = "Apple"
os = "macos"
program = "just"
steps = [{ args = ["test"], label = "suite" }]
role = "platforms"
kinds = ["nightly"]
kinds_github = ["nightly"]
timeout_minutes = 30
"#,
        );

        let error = KitharaExt::from_ctx(&ctx)
            .expect("parse kithara extension")
            .ci
            .validate()
            .expect_err("a macOS lane may not claim a GitHub schedule");
        assert!(
            error.to_string().contains("the GitHub fleet is Linux"),
            "the error must name the fleet: {error}"
        );
    }

    #[test]
    fn an_unknown_publish_step_fails_the_config() {
        let ctx = ctx_from_config(
            r#"
[ext.release.channels.release]
steps = ["retaind"]
"#,
        );

        assert!(KitharaExt::from_ctx(&ctx).is_err());
    }

    #[test]
    fn an_unknown_asset_key_fails_the_config() {
        let ctx = ctx_from_config(
            r#"
[ext.release.packages.snapshot]
assets = ["mergd"]
"#,
        );

        assert!(KitharaExt::from_ctx(&ctx).is_err());
    }

    #[test]
    fn an_unknown_profile_name_is_an_error_not_a_default() {
        let ctx = ctx_from_config(
            r#"
[ext.release.packages.release]
assets = ["core", "merged"]
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");

        assert!(ext.release.package("snapshot").is_err());
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
core_asset = "KitharaFFIInternal.xcframework.zip"
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
    fn hook_section_is_required_and_typed() {
        let ctx = ctx_from_config(
            r#"
[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"
"#,
        );

        let ext = KitharaExt::from_ctx(&ctx).expect("parse kithara extension");
        let hook = ext.agent_hook().expect("resolve agent hook config");

        assert_eq!(hook.routes.len(), 1);
    }

    #[test]
    fn missing_hook_section_fails_resolution() {
        let ctx = ctx_from_config("");
        let ext = KitharaExt::from_ctx(&ctx).expect("parse empty extension");

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
    fn cache_policy_loads_across_schema_drift_it_does_not_own() {
        let (_temp, root) = config_root(
            r#"
[ext.xtask.cache]
extra_inputs = ["justfile"]
keep_generations = 2
generation_grace_secs = 3600

[ext.apple]
default_simulator = "iPhone 17 Pro Max"
field_from_a_later_schema = true

[[health.feature_invariants]]
when_feature = "resample"
always = ["kithara/resample-glide"]
"#,
        );

        let config = XtaskCacheConfig::load(&root).expect("load cache policy");

        assert_eq!(config.keep_generations, 2);
        assert_eq!(config.extra_inputs, [PathBuf::from("justfile")]);
    }

    #[test]
    fn unparsable_project_config_is_a_parse_error() {
        let (_temp, root) = config_root("this is not valid TOML\n");

        let error = XtaskCacheConfig::load(&root).expect_err("invalid TOML fails");

        assert!(format!("{error:#}").contains("parse"));
    }

    #[test]
    fn missing_self_cache_section_fails_resolution() {
        let (_temp, root) = config_root("[project]\nname = \"fixture\"\n");

        assert!(XtaskCacheConfig::load(&root).is_err());
    }

    #[test]
    fn owned_self_cache_section_rejects_unknown_fields() {
        let (_temp, root) = config_root(
            r#"
[ext.xtask.cache]
extra_inputs = []
keep_generations = 2
generation_grace_secs = 3600
typo = true
"#,
        );

        let error = XtaskCacheConfig::load(&root).expect_err("cache typo fails");

        assert!(format!("{error:#}").contains("typo"));
    }
}
