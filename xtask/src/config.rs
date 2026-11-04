use std::{
    collections::{BTreeMap, BTreeSet},
    iter,
    num::NonZeroU64,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{Ctx, common::project::ProjectConfig};
use serde::{Deserialize, Serialize};

use crate::consts;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct KitharaExt {
    pub(crate) android: AndroidConfig,
    pub(crate) apple: AppleConfig,
    pub(crate) ci: CiProjectConfig,
    pub(crate) release: ReleaseConfig,
    pub(crate) publish: PublishConfig,
    agent_hook: Option<AgentHookConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiProjectConfig {
    pub(crate) pins: PathBuf,
    pub(crate) pr_review_line_limit: Option<NonZeroU64>,
    pub(crate) lanes: BTreeMap<String, CiLaneConfig>,
    pub(crate) verdict: CiVerdictConfig,
    /// How long a lane slot keeps a build unit its builds stopped using,
    /// counted back from the slot's latest use.
    lane_unit_window_hours: u64,
}

impl Default for CiProjectConfig {
    fn default() -> Self {
        Self {
            pins: PathBuf::new(),
            pr_review_line_limit: None,
            lanes: BTreeMap::new(),
            verdict: CiVerdictConfig::default(),
            lane_unit_window_hours: consts::LANE_UNIT_WINDOW_HOURS,
        }
    }
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
    /// Every operating system the lane runs on. GitHub fan-out requires Linux;
    /// lanes needing a dedicated pool opt out through `kinds_github`.
    #[serde(deserialize_with = "one_or_many")]
    pub(crate) os: Vec<String>,
    pub(crate) tools: Vec<String>,
    /// Tool readiness commands run before installing tools or restoring caches.
    pub(crate) checks: Vec<CiLaneCheck>,
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
    /// only afford weekly. Omission uses `kinds`; an empty list leaves the
    /// lane to its dedicated workflow.
    pub(crate) kinds_github: Option<Vec<String>>,
    /// A stable GitHub runner label for lanes whose persistent build cache
    /// must stay on one runner slot. Empty keeps the lane on the shared pool.
    pub(crate) github_runner: Option<String>,
    /// The protected GitHub runner label used by this lane on `main`.
    pub(crate) github_runner_main: Option<String>,
    /// Immutable trusted Cargo target snapshot this lane restores before it
    /// builds. The key names a compatible Cargo invocation, not a revision.
    pub(crate) target_snapshot: Option<String>,
    /// Whether this lane publishes dependency sources on its scope's default
    /// branch. The client's credentials select the bucket on either host.
    #[serde(default)]
    pub(crate) publishes_sources: bool,
    pub(crate) timeout_minutes: u32,
    /// Whether the lane reads the history of the commit it checks out, as a
    /// comparison against an earlier revision does. Without it the lane runs
    /// on the tip alone.
    pub(crate) history: bool,
    pub(crate) artifact: Option<CiLaneArtifact>,
    /// The concurrency group a lane wanting the whole host queues in.
    pub(crate) queue: Option<String>,
    /// Lanes whose artifacts this one consumes. A lane with needs runs after
    /// them and only when at least one of them was selected.
    pub(crate) needs: Vec<String>,
}

impl CiLaneConfig {
    /// Whether the shared Linux fan-out can carry the lane.
    pub(crate) fn runs_on_linux(&self) -> bool {
        self.os.iter().any(|os| os == "linux")
    }
}

fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(os) => vec![os],
        OneOrMany::Many(os) => os,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiLaneStep {
    pub(crate) args: Vec<String>,
    pub(crate) label: String,
    /// What the step needs the executor to be, rather than to run: a build-job
    /// cap the container cannot exceed, the browser a harness would otherwise
    /// guess. A value may name the checkout with `{root}` and a pinned version
    /// with `{pin.<key>}`. It may not name the build directory, which the
    /// executor owns.
    pub(crate) env: BTreeMap<String, String>,
    /// The program for this step alone. A lane that installs a target before
    /// using it runs two, so the lane's own `program` is only the default.
    pub(crate) program: Option<String>,
    /// Arguments for one pipeline kind, replacing `args` there. A review ref
    /// and the default branch ask the same question of a gate; a quarantine
    /// run deliberately asks a narrower one.
    pub(crate) args_by_kind: BTreeMap<String, Vec<String>>,
    /// Repeats the step building only and fails the lane on any unit cargo
    /// would build again: the next job of this commit, in the same build
    /// directory, would build it too. Only a `just test run` step can ask.
    pub(crate) rebuild_check: bool,
}

impl CiLaneStep {
    /// The arguments the step runs with in pipeline `kind`.
    pub(crate) fn args_in(&self, kind: &str) -> &[String] {
        self.args_by_kind.get(kind).unwrap_or(&self.args)
    }

    /// What the step asks of the test suite when `lane` runs it with `args`
    /// through `just test run`; any other step asks nothing of it.
    pub(crate) fn suite_request<'a>(
        &self,
        lane: &CiLaneConfig,
        args: &'a [String],
    ) -> Option<&'a [String]> {
        let program = self.program.as_deref().unwrap_or(lane.program.as_str());
        match args {
            [test, run, request @ ..] if program == "just" && test == "test" && run == "run" => {
                Some(request)
            }
            _ => None,
        }
    }
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

/// A tool command that must succeed before the lane can build.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CiLaneCheck {
    pub(crate) args: Vec<String>,
    pub(crate) tool: String,
}

impl CiProjectConfig {
    /// How long a lane slot keeps a build unit its builds stopped using.
    pub(crate) fn lane_unit_window(&self) -> Duration {
        Duration::from_secs(self.lane_unit_window_hours.saturating_mul(60 * 60))
    }

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
            if lane.os.is_empty() {
                bail!("ext.ci.lanes.{name} must name the operating system it runs on");
            }
            if let Some(os) = lane
                .os
                .iter()
                .find(|os| !matches!(os.as_str(), "linux" | "macos" | "windows"))
            {
                bail!("ext.ci.lanes.{name}.os must be linux, macos or windows, got `{os}`");
            }
            // `kinds_github` is a statement that GitHub schedules this lane, and
            // GitHub's fan-out reaches one pool. A lane excluding Linux
            // would be refused at selection and never run, which is a lane
            // declared into a schedule it cannot reach - the failure this
            // catalog exists to make impossible, not one to restate quietly.
            if lane
                .kinds_github
                .as_ref()
                .is_some_and(|kinds| !kinds.is_empty())
                && !lane.runs_on_linux()
            {
                bail!(
                    "ext.ci.lanes.{name}.kinds_github schedules a `{}` lane, and the GitHub fleet is Linux",
                    lane.os.join(" or ")
                );
            }
            if lane.github_runner.as_deref().is_some_and(str::is_empty) {
                bail!("ext.ci.lanes.{name}.github_runner must not be empty");
            }
            if lane
                .github_runner_main
                .as_deref()
                .is_some_and(str::is_empty)
            {
                bail!("ext.ci.lanes.{name}.github_runner_main must not be empty");
            }
            if lane.target_snapshot.as_deref().is_some_and(str::is_empty) {
                bail!("ext.ci.lanes.{name}.target_snapshot must not be empty");
            }
            if lane.publishes_sources
                && !matches!(lane.os.as_slice(), [os] if matches!(os.as_str(), "linux" | "macos"))
            {
                bail!(
                    "ext.ci.lanes.{name}.publishes_sources requires one supported native platform (linux or macos)"
                );
            }
            if lane.label.is_empty() {
                bail!("ext.ci.lanes.{name} must carry a label to refuse under");
            }
            for check in &lane.checks {
                if check.tool.is_empty() {
                    bail!("ext.ci.lanes.{name}.checks must name a tool");
                }
            }
            if !lane.left_behind.is_empty() && lane.left_behind_by.is_empty() {
                bail!("ext.ci.lanes.{name} must name the job its left_behind paths come from");
            }
            for step in &lane.steps {
                validate_step(name, lane, step)?;
            }
            if !consts::LANE_ROLES.contains(&lane.role.as_str()) {
                bail!(
                    "ext.ci.lanes.{name}.role must be one of {}, got `{}`",
                    consts::LANE_ROLES.join(", "),
                    lane.role
                );
            }
            for (field, listed) in [
                ("kinds", lane.kinds.as_slice()),
                (
                    "kinds_github",
                    lane.kinds_github.as_deref().unwrap_or_default(),
                ),
            ] {
                for kind in listed {
                    if !consts::PIPELINE_KINDS.contains(&kind.as_str()) {
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
        if self.lane_unit_window_hours == 0 {
            bail!(
                "ext.ci.lane_unit_window_hours must be at least one hour; at zero a lane's \
                 build directory would keep only the unit its latest build touched last"
            );
        }
        Ok(())
    }
}

/// A step spells only the substitutions the lane understands, and leaves the
/// build directory to the executor: the compiler cache keys every compilation on each `CARGO_*`
/// value, so a step that names its build directory splits every key it builds
/// by checkout.
fn validate_step(name: &str, lane: &CiLaneConfig, step: &CiLaneStep) -> Result<()> {
    for (key, value) in &step.env {
        validate_substitutions(name, key, value)?;
    }
    let by_kind = step.args_by_kind.values().flatten();
    for value in step.args.iter().chain(by_kind) {
        validate_substitutions(name, "an argument", value)?;
    }
    if step.env.contains_key(consts::TARGET_DIR_ENV) {
        bail!(
            "ext.ci.lanes.{name} sets {TARGET_DIR_ENV} in a step; the executor owns the build directory",
            TARGET_DIR_ENV = consts::TARGET_DIR_ENV
        );
    }
    if step.rebuild_check {
        let runs_the_suite = iter::once(&step.args)
            .chain(step.args_by_kind.values())
            .all(|args| step.suite_request(lane, args).is_some());
        if !runs_the_suite {
            bail!("ext.ci.lanes.{name}: a rebuild_check repeats a `just test run` step");
        }
    }
    Ok(())
}

/// `{root}` and `{pin.<key>}` are the whole substitution vocabulary. A typo
/// that reached the runner would be passed through as a literal brace and fail
/// as a missing header or an unknown toolchain rather than as a bad config.
fn validate_substitutions(lane: &str, whose: &str, value: &str) -> Result<()> {
    let mut rest = value.replace(consts::ROOT_PLACEHOLDER, "");
    while let Some(start) = rest.find(consts::PIN_PREFIX) {
        let Some(end) = rest[start..].find('}') else {
            bail!(
                "ext.ci.lanes.{lane} leaves {PIN_PREFIX} unclosed in {whose}: `{value}`",
                PIN_PREFIX = consts::PIN_PREFIX
            );
        };
        rest.replace_range(start..=start + end, "");
    }
    if rest.contains('{') {
        bail!(
            "ext.ci.lanes.{lane} names something other than {ROOT_PLACEHOLDER} or \
             {PIN_PREFIX}<key>}} in {whose}: `{value}`",
            PIN_PREFIX = consts::PIN_PREFIX,
            ROOT_PLACEHOLDER = consts::ROOT_PLACEHOLDER
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
    pub(crate) test_lane: String,
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
pub(crate) struct ReleaseConfig {
    /// Swift package manifest the tag step stamps with the version and the
    /// built framework's checksum, and the publish steps read at the tag.
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
    /// Documentation channels, keyed by the platform that renders them. Each
    /// names the directory a `doc` recipe writes, the zip it is published as,
    /// and where the Pages site serves it.
    pub(crate) docs: BTreeMap<String, DocsChannel>,
    /// WebAssembly channel: zip name for the trunk `dist` bundle the Pages site
    /// serves at its root, with the release section added to the player page.
    pub(crate) wasm_asset: String,
    /// Workspace-relative trunk `dist` dir zipped into [`Self::wasm_asset`]
    /// (the `just platform wasm build` output).
    pub(crate) wasm_dist: String,
    /// Branch GitHub Pages classic serves from (force-orphan deploy of the
    /// site).
    pub(crate) pages_branch: String,
    /// How `CHANGELOG.md` is rendered from commit subjects.
    pub(crate) changelog: ChangelogConfig,
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

    pub(crate) fn docs_channel(&self, name: &str) -> Result<&DocsChannel> {
        self.docs
            .get(name)
            .with_context(|| format!("ext.release.docs.{name} is not defined"))
    }

    /// Every published documentation zip, in a stable order.
    pub(crate) fn docs_assets(&self) -> impl Iterator<Item = &str> {
        self.docs
            .values()
            .map(|channel| channel.asset.as_str())
            .filter(|name| !name.is_empty())
    }

    /// Every artifact the release build jobs hand to the publish job, in the
    /// order a release lists them.
    pub(crate) fn assets(&self) -> impl Iterator<Item = &str> {
        [
            self.core_asset.as_str(),
            self.merged_asset.as_str(),
            self.wasm_asset.as_str(),
        ]
        .into_iter()
        .chain(self.platform_assets.iter().map(String::as_str))
        .chain(self.docs_assets())
        .filter(|name| !name.is_empty())
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
            AssetKey::Wasm => &self.wasm_asset,
        }
    }
}

/// One rendered documentation set: the directory a `doc` recipe writes, the
/// zip that directory is published as, and where the Pages site serves it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DocsChannel {
    /// Zip name uploaded as a release asset.
    pub(crate) asset: String,
    /// Workspace-relative directory zipped into [`Self::asset`].
    pub(crate) archive: String,
    /// Name the Pages site lists the documentation under.
    pub(crate) label: String,
    /// Pages site path the archive's contents are served under.
    pub(crate) pages_path: String,
    /// Page inside [`Self::pages_path`] a reader lands on.
    pub(crate) entry: String,
}

/// The git-cliff render of `CHANGELOG.md`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ChangelogConfig {
    /// Workspace-relative git-cliff configuration.
    pub(crate) config: String,
    /// Workspace-relative file the render is written to.
    pub(crate) output: String,
    /// Release tag the rendered history starts after; everything before it is
    /// hand-written in the configuration's footer.
    pub(crate) base: String,
}

/// One packaged artifact, named by what it carries rather than by file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssetKey {
    Core,
    Merged,
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
    Tag,
    Retained,
    NightlyRetained,
    Pages,
    Crates,
}

/// What a delivery channel requires before it runs and what it then performs.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ChannelProfile {
    /// Whether `KITHARA_RELEASE_VERSION` must be set and agree with every
    /// published crate's version at the built commit.
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
    /// New crates crates.io registers for one uploader at once.
    pub(crate) new_crate_burst: Option<usize>,
    /// Seconds crates.io makes one uploader wait between new crates once the
    /// burst is spent.
    pub(crate) new_crate_interval_secs: Option<u64>,
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
    use crate::consts;

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

    /// A workspace declaring one lane `suite` with the given step, and the
    /// given further keys in the lane's table.
    fn lane_config(keys: &str, step: &str) -> Ctx {
        ctx_from_config(&format!(
            r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.suite]
cache_group = "linux"
label = "Linux"
os = "linux"
program = "just"
{keys}
steps = [{step}]
role = "gate"
timeout_minutes = 30
"#
        ))
    }

    /// A lane's build directory follows one checkout, so the mtimes cargo
    /// reads are honest and no lane chooses another way to judge them.
    #[test]
    fn a_lane_does_not_choose_how_its_build_is_judged_fresh() {
        let error = KitharaExt::from_ctx(&lane_config(
            r#"freshness = "checksum""#,
            r#"{ args = ["test", "run"], label = "suite" }"#,
        ))
        .expect_err("freshness is no lane's choice");

        assert!(format!("{error:#}").contains("freshness"), "{error:#}");
    }

    /// A rebuild check repeats a `just test run` step building only; any other
    /// step has no cargo status lines to read.
    #[test]
    fn a_rebuild_check_repeats_a_suite_alone() {
        let validate = |step: &str| {
            KitharaExt::from_ctx(&lane_config("", step))
                .expect("parse kithara extension")
                .ci
                .validate()
        };

        validate(
            r#"{ args = ["test", "run", "--timings"], label = "suite", rebuild_check = true }"#,
        )
        .expect("a suite checks its rebuild");
        for step in [
            r#"{ args = ["lint"], label = "lint", rebuild_check = true }"#,
            r#"{ args = ["test", "run"], label = "suite", rebuild_check = true, args_by_kind = { quarantine = ["lint"] } }"#,
            r#"{ args = ["test", "run"], label = "suite", rebuild_check = true, program = "cargo" }"#,
        ] {
            let error = validate(step).expect_err(step);
            assert!(error.to_string().contains("rebuild_check"), "{error}");
        }
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
    fn a_release_channel_tags_before_it_publishes() {
        let ctx = ctx_from_config(
            r#"
[ext.release.channels.release]
requires_version = true
require_all_assets = true
tokens = ["CARGO_REGISTRY_TOKEN", "GH_TOKEN", "GITLAB_TOKEN"]
steps = ["tag", "retained", "pages", "crates"]

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
                PublishStep::Tag,
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
    fn only_a_supported_native_lane_may_publish_the_source_layer() {
        let declared = |cache_group: &str, os: &str| {
            let ctx = ctx_from_config(&format!(
                r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.publisher]
cache_group = "{cache_group}"
label = "Publisher"
os = {os}
program = "just"
role = "gate"
timeout_minutes = 60
steps = [{{ args = ["lint", "full"], label = "lint" }}]
publishes_sources = true
"#
            ));
            KitharaExt::from_ctx(&ctx)
                .expect("parse kithara extension")
                .ci
                .validate()
        };

        for (cache_group, os) in [
            ("linux", r#""linux""#),
            ("macos", r#""macos""#),
            ("linux", r#"["linux"]"#),
            ("macos", r#"["macos"]"#),
        ] {
            declared(cache_group, os).expect("a supported native lane may publish");
        }
        for (cache_group, os) in [
            ("windows", r#""windows""#),
            ("host", r#"["macos", "linux"]"#),
            ("host", r#"["linux", "windows"]"#),
            ("host", r#"["macos", "windows"]"#),
        ] {
            let refusal = declared(cache_group, os).expect_err(os);
            assert!(
                refusal.to_string().contains("publishes_sources"),
                "the refusal must name the field: {refusal}"
            );
        }
    }

    /// Every CI role renders its lanes through this validation before any lane
    /// starts, so a shipped config it refuses fails the whole run with no lane
    /// reporting a verdict.
    #[test]
    fn the_shipped_ci_config_validates() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask sits beside the workspace root")
            .to_path_buf();
        KitharaExt::load(&root)
            .expect("parse the shipped kithara extension")
            .ci
            .validate()
            .unwrap_or_else(|error| panic!("the shipped [ext.ci] is refused: {error:#}"));
    }

    #[test]
    fn a_lane_may_name_every_operating_system_it_runs_on() {
        let ctx = ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.device]
cache_group = "host"
label = "Android"
os = ["macos", "linux"]
program = "just"
steps = [{ args = ["test"], label = "suite" }]
role = "platforms"
kinds = ["nightly"]
timeout_minutes = 30
"#,
        );

        let ci = KitharaExt::from_ctx(&ctx)
            .expect("parse kithara extension")
            .ci;
        ci.validate().expect("a lane may run on two machines");
        assert_eq!(ci.lanes["device"].os, ["macos", "linux"]);
        assert!(ci.lanes["device"].runs_on_linux());
    }

    /// The compiler cache keys every compilation on each `CARGO_*` value, so a
    /// step that names its build directory, even the checkout's own `target`,
    /// splits every key it builds by checkout. The executor owns the build
    /// directory: Cargo finds `<checkout>/target` by itself.
    #[test]
    fn a_lane_step_may_not_name_the_build_directory() {
        for target in ["{root}/target", "/elsewhere"] {
            let ctx = ctx_from_config(&format!(
                r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.suite]
cache_group = "linux"
label = "Linux"
os = "linux"
program = "just"
steps = [{{ args = ["test"], label = "suite", env = {{ {name} = "{target}" }} }}]
role = "gate"
timeout_minutes = 30
"#,
                name = consts::TARGET_DIR_ENV,
            ));

            let error = KitharaExt::from_ctx(&ctx)
                .expect("parse kithara extension")
                .ci
                .validate()
                .expect_err("the executor owns the build directory");
            assert!(
                error.to_string().contains(consts::TARGET_DIR_ENV),
                "the refusal must name the variable: {error}"
            );
        }
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
test_lane = "android"
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
        assert_eq!(ext.android.test_lane, "android");
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

    #[test]
    fn a_lane_slot_keeps_a_day_of_units_unless_the_project_says_otherwise() {
        let unset = KitharaExt::from_ctx(&ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"
"#,
        ))
        .expect("parse kithara extension");
        let named = KitharaExt::from_ctx(&ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"
lane_unit_window_hours = 6
"#,
        ))
        .expect("parse kithara extension");

        assert_eq!(unset.ci.lane_unit_window_hours, 24);
        assert_eq!(named.ci.lane_unit_window_hours, 6);
        named.ci.validate().expect("a window of hours is valid");
    }

    #[test]
    fn a_lane_slot_window_of_zero_hours_is_refused() {
        let ext = KitharaExt::from_ctx(&ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"
lane_unit_window_hours = 0
"#,
        ))
        .expect("parse kithara extension");

        let error = ext
            .ci
            .validate()
            .expect_err("a zero window keeps nothing but the latest build");
        assert!(
            error.to_string().contains("lane_unit_window_hours"),
            "the error must name the key: {error}"
        );
    }

    #[test]
    fn github_membership_parsing_keeps_absence_empty_and_nonempty_distinct() {
        for (declaration, expected) in [
            ("", None),
            ("kinds_github = []", Some(Vec::new())),
            (
                "kinds_github = [\"main\", \"nightly\"]",
                Some(vec!["main".to_owned(), "nightly".to_owned()]),
            ),
        ] {
            let lane: super::CiLaneConfig = toml::from_str(&format!(
                "kinds = [\"weekly\"]\nhistory = true\n{declaration}\n"
            ))
            .expect("the lane parses");
            assert_eq!(lane.kinds_github, expected);
            assert_eq!(lane.kinds, ["weekly"]);
            assert!(lane.history);
        }
    }

    #[test]
    fn an_explicit_github_declination_allows_a_gitlab_only_machine() {
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
kinds = ["weekly"]
kinds_github = []
timeout_minutes = 30
history = true
"#,
        );
        let ci = KitharaExt::from_ctx(&ctx)
            .expect("parse kithara extension")
            .ci;
        ci.validate().expect("GitLab owns the macOS schedule");
        assert_eq!(ci.lanes["apple-thing"].kinds_github, Some(Vec::new()));
        assert!(ci.lanes["apple-thing"].history);
    }

    #[test]
    fn an_explicit_github_membership_still_rejects_unknown_kinds() {
        let ctx = ctx_from_config(
            r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.suite]
cache_group = "linux"
label = "Linux"
os = "linux"
program = "just"
steps = [{ args = ["test"], label = "suite" }]
role = "gate"
kinds = ["weekly"]
kinds_github = ["unknown"]
timeout_minutes = 30
"#,
        );
        let error = KitharaExt::from_ctx(&ctx)
            .expect("parse kithara extension")
            .ci
            .validate()
            .expect_err("an explicit membership must use known pipeline kinds");
        assert!(
            error
                .to_string()
                .contains("kinds_github names unknown kind"),
            "{error}"
        );
    }
}
