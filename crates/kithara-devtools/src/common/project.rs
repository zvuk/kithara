use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml::Table;

const CONFIG_REL: &str = ".config/xtask.toml";

/// Project-specific identity and per-tool settings for the otherwise
/// project-agnostic xtask. Loaded from `.config/xtask.toml`; every field
/// defaults to empty so a fresh project starts with no baked-in names and
/// fills in only what it uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub architecture: ArchitectureConfig,
    pub audit_clippy: AuditClippyConfig,
    pub ci_report: CiReportConfig,
    pub health: HealthConfig,
    pub lint_exclude: LintExcludeConfig,
    pub orphans: OrphansConfig,
    pub perf: PerfConfig,
    pub project: ProjectIdentity,
    pub quality: QualityConfig,
    pub stress: StressConfig,
    #[serde(default)]
    pub ext: Table,
    pub test: TestCommandConfig,
    pub tools: crate::common::tools::ToolsConfig,
    #[serde(default, rename = "workspace-scan")]
    pub workspace_scan: WorkspaceScan,
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureConfig {
    pub filters: ArchitectureFilterConfig,
    pub render: ArchitectureRenderBudgets,
    pub runtime: ArchitectureRuntimeConfig,
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureFilterConfig {
    pub exclude_crates: Vec<String>,
    pub exclude_modules: Vec<String>,
}

/// How much of a diagram's finding set the architecture report renders.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureRenderBudgets {
    pub findings: usize,
    pub relations: usize,
}

impl Default for ArchitectureRenderBudgets {
    fn default() -> Self {
        Self {
            findings: 12,
            relations: 20,
        }
    }
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureRuntimeConfig {
    pub scenarios: Vec<RuntimeScenarioConfig>,
    /// Wall-clock budget for the rust-analyzer session the semantic overlay
    /// runs on. Workspace loading and every call-hierarchy request share the
    /// one deadline, so the overlay reports `timed_out` rather than holding
    /// the visualization open indefinitely.
    #[serde(default = "default_semantic_timeout_secs")]
    pub semantic_timeout_secs: u64,
}

impl Default for ArchitectureRuntimeConfig {
    fn default() -> Self {
        Self {
            scenarios: Vec::new(),
            semantic_timeout_secs: default_semantic_timeout_secs(),
        }
    }
}

const fn default_semantic_timeout_secs() -> u64 {
    120
}

#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RuntimeScenarioConfig {
    Test {
        name: String,
        package: String,
        test: String,
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        ignored: bool,
        timeout_secs: u64,
    },
    Binary {
        name: String,
        package: String,
        bin: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        features: Vec<String>,
        timeout_secs: u64,
    },
    Trace {
        name: String,
        path: String,
    },
}

impl RuntimeScenarioConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Test { name, .. } | Self::Binary { name, .. } | Self::Trace { name, .. } => name,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.name().is_empty()
            || !self.name().chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            bail!(
                "architecture runtime scenario name `{}` must use only ASCII letters, digits, '-' or '_'",
                self.name()
            );
        }
        match self {
            Self::Test {
                package,
                test,
                timeout_secs,
                ..
            } => {
                if package.is_empty() || test.is_empty() {
                    bail!(
                        "architecture runtime test scenario `{}` requires package and test",
                        self.name()
                    );
                }
                if *timeout_secs == 0 {
                    bail!(
                        "architecture runtime scenario `{}` timeout_secs must be positive",
                        self.name()
                    );
                }
            }
            Self::Binary {
                package,
                bin,
                timeout_secs,
                ..
            } => {
                if package.is_empty() || bin.is_empty() {
                    bail!(
                        "architecture runtime binary scenario `{}` requires package and bin",
                        self.name()
                    );
                }
                if *timeout_secs == 0 {
                    bail!(
                        "architecture runtime scenario `{}` timeout_secs must be positive",
                        self.name()
                    );
                }
            }
            Self::Trace { path, .. } if path.is_empty() => {
                bail!(
                    "architecture runtime trace scenario `{}` requires path",
                    self.name()
                );
            }
            Self::Trace { .. } => {}
        }
        Ok(())
    }
}

/// Extended advisory clippy lints for the opt-in `just lint audit-clippy` sweep.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuditClippyConfig {
    pub lints: Vec<String>,
}

/// How much of each measurement the consolidated CI report inlines before it
/// sends the reader to the artifact it was rendered from.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct CiReportConfig {
    /// Rows of the CRAP table carried into the report. The whole table runs to
    /// five figures of lines and a step summary is capped at a megabyte.
    pub crap_rows: usize,
    /// Lines of the duplication report carried into the report. It leads with
    /// the crate-level map and the explainable candidates, which is the part
    /// worth reading without opening the artifact.
    pub similarity_rows: usize,
    /// Contours listed under the architecture complexity index, worst first.
    pub top_contours: usize,
}

impl Default for CiReportConfig {
    fn default() -> Self {
        Self {
            crap_rows: 120,
            top_contours: 10,
            similarity_rows: 80,
        }
    }
}

/// Workspace-wide Rust file scan exclusions.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceScan {
    pub exclude: Vec<String>,
    /// Directories a scope token may name outside `crates/`. A token whose
    /// first component is one of these resolves to a workspace path rather
    /// than to a crate.
    #[serde(default = "default_top_level_dirs")]
    pub top_level_dirs: Vec<String>,
}

impl Default for WorkspaceScan {
    fn default() -> Self {
        Self {
            exclude: Vec::new(),
            top_level_dirs: default_top_level_dirs(),
        }
    }
}

fn default_top_level_dirs() -> Vec<String> {
    ["tests", "xtask", "benches"].map(String::from).to_vec()
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrphansConfig {
    /// Packages excluded from the default `cargo modules orphans` sweep
    /// (generated/helper/macro crates and per-target-gated crates that the
    /// default rust-analyzer view flags as false-positive orphans).
    pub exclude_packages: Vec<String>,
    /// Upper bound on concurrent `cargo modules` runs. Each holds a whole
    /// rust-analyzer database, so the sweep is capped by what the job's
    /// memory holds as well as by its cores.
    #[serde(default = "default_orphans_max_parallelism")]
    pub max_parallelism: usize,
}

impl Default for OrphansConfig {
    fn default() -> Self {
        Self {
            exclude_packages: Vec::new(),
            max_parallelism: default_orphans_max_parallelism(),
        }
    }
}

const fn default_orphans_max_parallelism() -> usize {
    4
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityConfig {
    /// Repository-specific heavyweight stages used by `quality assess --depth deep`.
    pub assessment: QualityAssessmentConfig,
    pub render: QualityRenderBudgets,
    /// Trait directory whose every `pub trait` must carry workspace mock coverage.
    pub unimock_traits_dir: String,
}

/// How much of a quality assessment the artifact and the summary render.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct QualityRenderBudgets {
    pub architecture_hotspots: usize,
    pub findings: usize,
    pub summary_rows: usize,
}

impl Default for QualityRenderBudgets {
    fn default() -> Self {
        Self {
            architecture_hotspots: 30,
            findings: 100,
            summary_rows: 5,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentConfig {
    pub deep_stages: Vec<QualityAssessmentStageConfig>,
    pub not_applicable_tools: Vec<QualityAssessmentToolPolicyConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentStageConfig {
    pub name: String,
    pub command: Vec<String>,
    pub expected_artifacts: Vec<String>,
    pub platforms: Vec<String>,
    pub tools: Vec<String>,
    pub complete_only: bool,
    pub hard_invariant: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityAssessmentToolPolicyConfig {
    pub reason: String,
    pub tool: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LintExcludeConfig {
    /// Inline-module names / `::`-paths whose violations are dropped from every
    /// lint namespace, regardless of file.
    pub modules: Vec<String>,
    /// Workspace-relative globs whose violations are dropped from every lint
    /// namespace (`arch`, `style`, `idioms`) so baselines measure production
    /// debt, not test code. `#[cfg(test)]` blocks are stripped automatically
    /// (AST) on top of this — no glob can match inline test modules.
    pub paths: Vec<String>,
    /// ast-grep rule IDs that must scan the FULL tree — tests included —
    /// bypassing [`Self::paths`]. Hard-correctness bans (e.g. `arch.no-direct-time`)
    /// where test code is NOT exempt: routing time through one primitive only
    /// works if tests obey it too. Run in a second ast-grep pass per rule with
    /// no exclude globs; the rule's own `files:` / `ignores:` scope it.
    pub scan_all_rules: Vec<String>,
    /// Build tooling, dropped by [`Self::runtime_paths`] alone: it is not a
    /// runtime path, so architecture and idiom rules have nothing to say about
    /// it, and their lexical rules misfire on the lint engine's own sources,
    /// which carry the patterns they detect. `style` keeps these files.
    pub tooling_paths: Vec<String>,
}

impl LintExcludeConfig {
    /// What `arch`, `idioms`, and ast-grep drop: test code plus build tooling.
    /// `style` applies [`Self::paths`] alone, so tooling source stays under the
    /// comment, document, and ordering rules.
    #[must_use]
    pub fn runtime_paths(&self) -> Vec<String> {
        let mut out = self.paths.clone();
        out.extend(self.tooling_paths.iter().cloned());
        out
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectIdentity {
    /// Used in human-facing labels: health report title, temp-log prefix.
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Package whose dependency closure the unsafe-code census is rooted at.
    pub geiger_package: String,
    /// Directory the per-stage logs are written to.
    #[serde(default = "default_health_logs_dir")]
    pub logs_dir: String,
    /// Document the run's verdict and per-stage log tails are written to.
    #[serde(default = "default_health_report_path")]
    pub report_path: String,
    /// Backend groups a crate refuses to be built without.
    pub feature_invariants: Vec<FeatureInvariant>,
    /// Crates excluded from the `cargo hack --feature-powerset` stage.
    pub feature_powerset_exclude: Vec<String>,
    /// Crates whose deadlock findings the stage reports without failing on.
    /// Only this workspace's own crates belong here: a dependency is out of
    /// the verdict already, and this list is for a finding that has an owner
    /// and a place it is being fixed.
    pub lockbud_exclude: Vec<String>,
    /// Crates whose manifest a generator owns, so "is this dependency used?"
    /// is a question about the generator rather than about the code.
    pub machete_exclude: Vec<String>,
    /// Packages the semver stage compares against the baseline branch.
    pub semver_packages: Vec<String>,
    /// Trailing log lines each stage inlines into the report before it sends
    /// the reader to the full log on disk.
    #[serde(default = "default_health_stdout_tail_lines")]
    pub stdout_tail_lines: usize,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            geiger_package: String::new(),
            logs_dir: default_health_logs_dir(),
            report_path: default_health_report_path(),
            feature_invariants: Vec::new(),
            feature_powerset_exclude: Vec::new(),
            lockbud_exclude: Vec::new(),
            machete_exclude: Vec::new(),
            semver_packages: Vec::new(),
            stdout_tail_lines: default_health_stdout_tail_lines(),
        }
    }
}

fn default_health_logs_dir() -> String {
    "target/health-logs".to_owned()
}

fn default_health_report_path() -> String {
    "target/health-report.md".to_owned()
}

const fn default_health_stdout_tail_lines() -> usize {
    80
}

/// A rule some crates state with `compile_error!`: this build needs a backend.
///
/// The crate it applies to is not named here. It is found by the feature that
/// names the group, so the rule follows the workspace rather than being
/// repeated beside it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureInvariant {
    /// Feature whose presence marks a crate as carrying this rule.
    pub when_feature: String,
    /// Features every combination carries. A group of one is expressed here:
    /// `--at-least-one-of` needs two or more names, and where only one backend
    /// survives its target gate there is nothing to choose between.
    pub always: Vec<String>,
    /// Groups the powerset must pick from rather than leave empty.
    pub at_least_one_of: Vec<Vec<String>>,
}

impl FeatureInvariant {
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for group in &self.at_least_one_of {
            args.push("--at-least-one-of".to_owned());
            args.push(group.join(","));
        }
        if !self.always.is_empty() {
            args.push("--features".to_owned());
            args.push(self.always.join(","));
        }
        args
    }
}

#[derive(Clone, Debug, Deserialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct PerfConfig {
    pub frame_prefix: Option<String>,
    #[serde(default = "default_perf_nextest_profile")]
    pub nextest_profile: String,
    pub primary_lane: String,
    pub lanes: Vec<PerfLane>,
}

impl Default for PerfConfig {
    fn default() -> Self {
        Self {
            lanes: Vec::new(),
            primary_lane: String::new(),
            frame_prefix: None,
            nextest_profile: default_perf_nextest_profile(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct PerfLane {
    pub backend: String,
    pub flash: bool,
}

fn default_perf_nextest_profile() -> String {
    "perf".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestCommandConfig {
    pub lanes: BTreeMap<String, TestLaneConfig>,
    pub net_backends: BTreeMap<String, TestNetBackendConfig>,
    pub default_backend: String,
    pub default_lane: String,
    pub feature_arg: String,
    pub loom_lane: String,
    pub flash: TestFlashConfig,
    pub no_block: TestNoBlockConfig,
    pub features: Vec<String>,
    /// Paths that belong to no single lane: a change to one of them runs every
    /// lane that declares `owns`, because the routing itself moved.
    pub shared_paths: Vec<String>,
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
pub struct TestNoBlockConfig {
    pub features: Vec<String>,
    pub default: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestNetBackendConfig {
    pub features: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestLaneConfig {
    /// Environment the lane runs with, so what the lane exercises is named by
    /// the lane rather than by whatever the caller happened to export.
    pub env: BTreeMap<String, String>,
    pub default_flash: Option<bool>,
    /// Poll-blocking detector default for this lane, so two schedulers cannot
    /// run the same lane under different rules.
    pub default_no_block: Option<bool>,
    pub passthrough: String,
    pub program: String,
    pub default_features: Vec<String>,
    /// Source prefixes this lane is the test for. `just test run --touched`
    /// runs the lane when the branch changed a path under one of them; a lane
    /// that owns nothing is never selected that way.
    pub owns: Vec<String>,
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressConfig {
    pub modes: BTreeMap<String, StressModeConfig>,
    pub artifacts: StressArtifactConfig,
    pub environment: StressEnvironmentConfig,
    pub evidence: StressEvidenceConfig,
    pub render: StressRenderBudgets,
    pub backend: String,
    /// The directory a lane builds into, relative to the checkout it builds.
    ///
    /// A stress run that inherits `CARGO_TARGET_DIR` builds into whatever
    /// directory the machine shares with everything else on it, and a stress
    /// run lasts hours: five of them lost a whole lane when those binaries
    /// disappeared mid-run and every remaining repeat failed to exec in
    /// milliseconds. Naming the directory here is what makes the artifacts the
    /// lane runs belong to the revision the lane was asked about.
    pub build_dir: String,
    pub default_filter: String,
    pub lane: String,
    pub nextest_config: String,
    pub nextest_profile: String,
    pub raw_output: String,
    pub report_output: String,
    pub test_threads: String,
    /// The lanes one run is made of, executed in order. More than one is the
    /// normal case: a clock the fixtures' delays collapse under answers a
    /// different question than a clock they survive, and a run covering
    /// only one of them cannot say which of the two a flake belongs to.
    pub default_modes: Vec<String>,
    pub workflow_job_timeout_minutes: u64,
    pub default_count: usize,
    pub max_count: usize,
    pub max_test_threads: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressArtifactConfig {
    pub envelope_dir: Option<String>,
    pub line_log: Option<String>,
    /// Per-attempt exit codes of a lane that repeats a command. A sanitizer
    /// leaves no per-test verdict, so this is the whole of what such a lane
    /// can be counted by.
    pub attempts: String,
    pub inventory: String,
    pub junit: String,
    pub log: String,
    pub manifest: String,
    pub pressure: String,
    pub report: String,
    /// Where the test runner leaves its report, relative to the subject
    /// checkout.
    ///
    /// nextest's store is rooted at the workspace root and does not follow
    /// `CARGO_TARGET_DIR`, so the report stays put while the build is sent to
    /// the run's own directory.
    pub subject_junit: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressEnvironmentConfig {
    pub remove: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressModeConfig {
    pub raw_path_env: BTreeMap<String, String>,
    pub set_env: BTreeMap<String, String>,
    /// Where this command leaves a `JUnit` report, relative to the checkout
    /// root rather than to `build_dir`: the runner's store anchors on the
    /// workspace it tests, not on the directory it builds into.
    ///
    /// An exit code names no test. When the command runs its tests under a
    /// runner that writes a report anyway, that report is what turns "something
    /// aborted" into "this test aborted, this often". The runner overwrites the
    /// file every attempt, so the lane keeps a copy of each.
    pub attempt_junit: Option<String>,
    /// A command this lane runs instead of the configured test runner.
    ///
    /// Some lanes cannot be described as a feature set: a sanitizer lane picks
    /// its own toolchain, compiler flags and runtime library, and that contract
    /// belongs to the recipe that owns it rather than to a second copy here.
    /// The run launches the command and reads what it leaves behind. Empty
    /// means the lane runs the configured test runner and is measured per test.
    pub command: Vec<String>,
    pub features: Vec<String>,
    /// Whether the command performs the run's repeats itself.
    ///
    /// A command that runs its tests under nextest can be handed the count
    /// through `KITHARA_STRESS_REPEATS` and launched once: the workspace builds
    /// once, and every repeat lands in one report carrying a per-test verdict,
    /// which is what lets the lane stand in the same comparison as the lanes the
    /// run drives directly. Launched once per repeat instead, the same lane
    /// pays a rebuild and a cold start each time and can report only an exit
    /// code — and an exit code names no test.
    pub owns_repeats: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressEvidenceConfig {
    pub dump_marker: Option<String>,
    pub envelope_marker: Option<String>,
    pub envelope_schema: Option<String>,
    pub envelope_text_field: Option<String>,
    pub holder_marker: Option<String>,
    pub line_marker: Option<String>,
    pub primitive_marker: Option<String>,
    pub wait_marker: Option<String>,
    pub direct_markers: Vec<String>,
    pub envelope_suffix_markers: Vec<String>,
    pub source_excludes: Vec<String>,
}

/// How much of a stress finding set the report asks a human to read.
///
/// A budget, not a guard: exceeding it truncates a table, it does not refuse an
/// artifact. The guards that refuse are the `MAX_*_BYTES` in `stress_report`
/// and stay in code.
///
/// `signature_examples` is the one that reads wider than its name. It bounds
/// the frames `backtrace_signature` folds into a failure's key, so lowering it
/// makes two failures that diverge deep in the stack cluster as one. It shapes
/// what the evidence says, not how wide a column is.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
#[serde(default, deny_unknown_fields)]
pub struct StressRenderBudgets {
    pub cell_chars: usize,
    pub divergence_rows: usize,
    pub failure_rows: usize,
    pub finding_rows: usize,
    pub iterations_per_test: usize,
    pub pass_only_rows_per_test: usize,
    pub problem_rows: usize,
    pub signature_examples: usize,
    pub signature_rows: usize,
}

impl Default for StressRenderBudgets {
    fn default() -> Self {
        Self {
            cell_chars: 240,
            divergence_rows: 40,
            failure_rows: 100,
            finding_rows: 100,
            iterations_per_test: 20,
            pass_only_rows_per_test: 5,
            problem_rows: 100,
            signature_examples: 5,
            signature_rows: 100,
        }
    }
}

impl StressConfig {
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self != &Self::default()
    }

    /// Resolve one configured stress mode.
    ///
    /// # Errors
    ///
    /// Returns an error when `name` is not configured.
    pub fn mode(&self, name: &str) -> Result<&StressModeConfig> {
        self.modes
            .get(name)
            .with_context(|| format!("stress mode `{name}` is not configured"))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        require_value("stress.lane", &self.lane)?;
        require_value("stress.backend", &self.backend)?;
        require_value("stress.nextest_config", &self.nextest_config)?;
        require_value("stress.nextest_profile", &self.nextest_profile)?;
        require_value("stress.default_filter", &self.default_filter)?;
        require_value("stress.test_threads", &self.test_threads)?;
        validate_relative_path("stress.nextest_config", &self.nextest_config)?;
        validate_relative_path("stress.build_dir", &self.build_dir)?;
        validate_relative_path("stress.raw_output", &self.raw_output)?;
        validate_relative_path("stress.report_output", &self.report_output)?;
        ensure_positive("stress.default_count", self.default_count)?;
        ensure_positive("stress.max_count", self.max_count)?;
        ensure_positive("stress.max_test_threads", self.max_test_threads)?;
        if self.workflow_job_timeout_minutes == 0 {
            bail!("stress.workflow_job_timeout_minutes must be positive");
        }
        if self.default_count > self.max_count {
            bail!("stress.default_count cannot exceed stress.max_count");
        }
        if self.test_threads != "num-cpus" {
            let test_threads = self
                .test_threads
                .parse::<usize>()
                .with_context(|| "stress.test_threads must be `num-cpus` or a positive integer")?;
            ensure_positive("stress.test_threads", test_threads)?;
            if test_threads > self.max_test_threads {
                bail!("stress.test_threads cannot exceed stress.max_test_threads");
            }
        }

        self.validate_artifacts()?;
        self.validate_evidence()?;
        let mut removed = BTreeSet::new();
        for key in &self.environment.remove {
            require_env_key("stress.environment.remove", key)?;
            if !removed.insert(key) {
                bail!("stress.environment.remove contains duplicate key `{key}`");
            }
        }
        for (name, mode) in &self.modes {
            require_value("stress mode name", name)?;
            Self::validate_mode(name, mode)?;
        }
        if self.default_modes.is_empty() {
            bail!("stress.default_modes must name at least one mode");
        }
        let mut seen = BTreeSet::new();
        for name in &self.default_modes {
            require_value("stress.default_modes entry", name)?;
            if !self.modes.contains_key(name) {
                bail!("stress.default_modes names `{name}`, which is not configured");
            }
            // A lane names the directory its evidence lands in, so a run
            // that listed one twice would have its second run overwrite the
            // first and report half of what it did.
            if !seen.insert(name) {
                bail!("stress.default_modes names `{name}` twice");
            }
        }
        Ok(())
    }

    fn validate_artifacts(&self) -> Result<()> {
        for (name, path) in [
            ("subject_junit", self.artifacts.subject_junit.as_str()),
            ("inventory", self.artifacts.inventory.as_str()),
            ("junit", self.artifacts.junit.as_str()),
            ("log", self.artifacts.log.as_str()),
            ("manifest", self.artifacts.manifest.as_str()),
            ("pressure", self.artifacts.pressure.as_str()),
            ("report", self.artifacts.report.as_str()),
            ("attempts", self.artifacts.attempts.as_str()),
        ] {
            validate_relative_path(&format!("stress.artifacts.{name}"), path)?;
        }
        if let Some(path) = &self.artifacts.envelope_dir {
            validate_relative_path("stress.artifacts.envelope_dir", path)?;
        }
        if let Some(path) = &self.artifacts.line_log {
            validate_relative_path("stress.artifacts.line_log", path)?;
        }
        Ok(())
    }

    fn validate_evidence(&self) -> Result<()> {
        match self.artifacts.envelope_dir.as_deref() {
            Some(_) => {
                require_value(
                    "stress.evidence.envelope_schema",
                    self.evidence
                        .envelope_schema
                        .as_deref()
                        .context("stress.evidence.envelope_schema is not configured")?,
                )?;
                require_value(
                    "stress.evidence.envelope_marker",
                    self.evidence
                        .envelope_marker
                        .as_deref()
                        .context("stress.evidence.envelope_marker is not configured")?,
                )?;
                if let Some(field) = &self.evidence.envelope_text_field {
                    require_value("stress.evidence.envelope_text_field", field)?;
                }
            }
            None => {
                if self.evidence.envelope_schema.is_some()
                    || self.evidence.envelope_marker.is_some()
                    || self.evidence.envelope_text_field.is_some()
                    || !self.evidence.envelope_suffix_markers.is_empty()
                {
                    bail!(
                        "stress.artifacts.envelope_dir, stress.evidence.envelope_schema, and stress.evidence.envelope_marker must be configured together"
                    );
                }
            }
        }
        match (
            self.artifacts.line_log.as_deref(),
            self.evidence.line_marker.as_deref(),
        ) {
            (Some(_), Some(marker)) => require_value("stress.evidence.line_marker", marker)?,
            (None, None) => {}
            _ => bail!(
                "stress.artifacts.line_log and stress.evidence.line_marker must be configured together"
            ),
        }
        for (name, marker) in [
            ("dump_marker", self.evidence.dump_marker.as_deref()),
            (
                "primitive_marker",
                self.evidence.primitive_marker.as_deref(),
            ),
            ("holder_marker", self.evidence.holder_marker.as_deref()),
            ("wait_marker", self.evidence.wait_marker.as_deref()),
        ] {
            if let Some(marker) = marker {
                require_value(&format!("stress.evidence.{name}"), marker)?;
            }
        }
        for marker in self
            .evidence
            .direct_markers
            .iter()
            .chain(&self.evidence.envelope_suffix_markers)
            .chain(&self.evidence.source_excludes)
        {
            require_value("stress evidence marker", marker)?;
        }
        Ok(())
    }

    fn validate_mode(name: &str, mode: &StressModeConfig) -> Result<()> {
        let mut features = BTreeSet::new();
        for feature in &mode.features {
            require_value(&format!("stress.modes.{name}.features"), feature)?;
            if !features.insert(feature) {
                bail!("stress mode `{name}` contains duplicate feature `{feature}`");
            }
        }
        for key in mode.set_env.keys() {
            require_env_key(&format!("stress.modes.{name}.set_env"), key)?;
            if mode.raw_path_env.contains_key(key) {
                bail!(
                    "stress mode `{name}` environment key `{key}` cannot be set as both a value and a raw path"
                );
            }
        }
        for (key, path) in &mode.raw_path_env {
            require_env_key(&format!("stress.modes.{name}.raw_path_env"), key)?;
            validate_relative_path(&format!("stress.modes.{name}.raw_path_env.{key}"), path)?;
        }
        for word in &mode.command {
            require_value(&format!("stress.modes.{name}.command"), word)?;
        }
        if let Some(path) = &mode.attempt_junit {
            validate_relative_path(&format!("stress.modes.{name}.attempt_junit"), path)?;
        }
        // A command lane selects nothing through the test runner, so features
        // meant for that runner would be read by no one. Saying so here beats
        // a lane that silently ignores half of what it was configured with.
        if !mode.command.is_empty() && !mode.features.is_empty() {
            bail!("stress mode `{name}` runs a command, so its features reach nothing");
        }
        Ok(())
    }
}

fn require_value(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(())
}

fn require_env_key(owner: &str, key: &str) -> Result<()> {
    if key.trim().is_empty() {
        bail!("{owner} contains an empty environment key");
    }
    Ok(())
}

fn ensure_positive(name: &str, value: usize) -> Result<()> {
    if value == 0 {
        bail!("{name} must be positive");
    }
    Ok(())
}

fn validate_relative_path(name: &str, value: &str) -> Result<()> {
    require_value(name, value)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("{name} must be a safe relative path");
    }
    Ok(())
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
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parse project config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for pattern in self
            .architecture
            .filters
            .exclude_crates
            .iter()
            .chain(&self.architecture.filters.exclude_modules)
        {
            if pattern.is_empty() {
                bail!("architecture exclusion glob cannot be empty");
            }
            glob::Pattern::new(pattern)
                .with_context(|| format!("invalid architecture exclusion glob `{pattern}`"))?;
        }
        let mut names = BTreeSet::new();
        for scenario in &self.architecture.runtime.scenarios {
            scenario.validate()?;
            if !names.insert(scenario.name()) {
                bail!(
                    "duplicate architecture runtime scenario `{}`",
                    scenario.name()
                );
            }
        }
        let mut stage_names = BTreeSet::new();
        let mut stage_tools = BTreeSet::new();
        for stage in &self.quality.assessment.deep_stages {
            if stage.name.is_empty()
                || !stage.name.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                bail!(
                    "quality assessment stage name `{}` must use only ASCII letters, digits, '-' or '_'",
                    stage.name
                );
            }
            if !stage_names.insert(&stage.name) {
                bail!("duplicate quality assessment stage `{}`", stage.name);
            }
            if stage.command.is_empty() {
                bail!(
                    "quality assessment stage `{}` requires a command",
                    stage.name
                );
            }
            if stage.tools.is_empty() {
                bail!(
                    "quality assessment stage `{}` requires at least one owned tool signal",
                    stage.name
                );
            }
            stage_tools.extend(stage.tools.iter().map(String::as_str));
        }
        let mut policy_tools = BTreeSet::new();
        for policy in &self.quality.assessment.not_applicable_tools {
            if policy.tool.is_empty()
                || !policy.tool.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                bail!(
                    "quality assessment policy tool `{}` must use only ASCII letters, digits, '-' or '_'",
                    policy.tool
                );
            }
            if policy.reason.trim().is_empty() {
                bail!(
                    "quality assessment policy tool `{}` requires a reason",
                    policy.tool
                );
            }
            if !policy_tools.insert(policy.tool.as_str()) {
                bail!("duplicate quality assessment policy tool `{}`", policy.tool);
            }
            if stage_tools.contains(policy.tool.as_str()) {
                bail!(
                    "quality assessment tool `{}` cannot be both configured and not applicable",
                    policy.tool
                );
            }
        }
        if self.orphans.max_parallelism == 0 {
            bail!(
                "orphans.max_parallelism must admit at least one worker; the sweep clamps its \
                 worker count into `1..=max_parallelism` and panics on an empty range"
            );
        }
        if self.stress.is_configured() {
            self.stress.validate()?;
            if !self.test.lanes.contains_key(&self.stress.lane) {
                bail!(
                    "stress.lane `{}` is not configured under test.lanes",
                    self.stress.lane
                );
            }
            if !self.test.net_backends.contains_key(&self.stress.backend) {
                bail!(
                    "stress.backend `{}` is not configured under test.net_backends",
                    self.stress.backend
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod architecture_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn load(text: &str) -> Result<ProjectConfig> {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("config dir");
        fs::write(temp.path().join(CONFIG_REL), text).expect("config");
        ProjectConfig::load(temp.path())
    }

    /// `workers` clamps into `1..=max_parallelism`, and `usize::clamp` panics
    /// when the range is empty, so zero reaches the sweep as an abort rather
    /// than as a setting.
    #[test]
    fn a_zero_orphan_worker_ceiling_is_refused() {
        let error = load(
            r#"
[orphans]
max_parallelism = 0
"#,
        )
        .expect_err("a ceiling below one worker");

        assert!(
            format!("{error:#}").contains("max_parallelism"),
            "{error:#}"
        );
    }

    #[test]
    fn architecture_runtime_config_is_strict() {
        let error = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "test"
package = "demo"
test = "architecture"
timeout_secs = 30
unknown = true
"#,
        )
        .expect_err("unknown field");

        assert!(error.to_string().contains("parse project config"));
    }

    #[test]
    fn architecture_runtime_names_and_timeouts_are_validated() {
        let duplicate = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "trace"
path = "first.jsonl"

[[architecture.runtime.scenarios]]
name = "demo"
command = "trace"
path = "second.jsonl"
"#,
        )
        .expect_err("duplicate");
        assert!(duplicate.to_string().contains("duplicate"));

        let zero = load(
            r#"
[[architecture.runtime.scenarios]]
name = "demo"
command = "test"
package = "demo"
test = "architecture"
timeout_secs = 0
"#,
        )
        .expect_err("zero timeout");
        assert!(zero.to_string().contains("must be positive"));
    }

    #[test]
    fn architecture_filters_are_strict_and_validate_globs() {
        let config = load(
            r#"
[architecture.filters]
exclude_crates = ["dev-*"]
exclude_modules = ["*::tests"]
"#,
        )
        .expect("architecture filters");
        assert_eq!(
            config.architecture.filters.exclude_crates,
            ["dev-*".to_string()]
        );
        assert_eq!(
            config.architecture.filters.exclude_modules,
            ["*::tests".to_string()]
        );

        let invalid = load(
            r#"
[architecture.filters]
exclude_crates = ["["]
"#,
        )
        .expect_err("invalid glob");
        assert!(
            invalid
                .to_string()
                .contains("invalid architecture exclusion glob")
        );

        let empty = load(
            r#"
[architecture.filters]
exclude_modules = [""]
"#,
        )
        .expect_err("empty glob");
        assert!(empty.to_string().contains("cannot be empty"));

        let unknown = load(
            r#"
[architecture.filters]
unknown = ["dev-*"]
"#,
        )
        .expect_err("unknown filter field");
        assert!(unknown.to_string().contains("parse project config"));
    }

    #[test]
    fn quality_assessment_deep_stages_are_loaded() {
        let config = load(
            r#"
[[quality.assessment.not_applicable_tools]]
tool = "cargo-mutants"
reason = "not actionable for this workspace"

[[quality.assessment.deep_stages]]
name = "mutation"
command = ["just", "test", "mutants", "ci", "target/mutants"]
tools = ["mutation-runner"]
expected_artifacts = ["target/mutants/mutants.out/outcomes.json"]
hard_invariant = true
complete_only = true
"#,
        )
        .expect("quality assessment config");

        let stage = &config.quality.assessment.deep_stages[0];
        assert_eq!(stage.name, "mutation");
        assert_eq!(stage.command[0], "just");
        assert_eq!(stage.tools, ["mutation-runner"]);
        assert!(stage.hard_invariant);
        assert!(stage.complete_only);
        let policy = &config.quality.assessment.not_applicable_tools[0];
        assert_eq!(policy.tool, "cargo-mutants");
        assert_eq!(policy.reason, "not actionable for this workspace");
    }

    #[test]
    fn runtime_paths_carry_the_tooling_globs_and_paths_do_not() {
        let config = load(
            r#"
[lint_exclude]
paths = ["**/tests/**"]
tooling_paths = ["crates/kithara-devtools/**"]
"#,
        )
        .expect("lint exclude config");

        assert_eq!(config.lint_exclude.paths, ["**/tests/**"]);
        assert_eq!(
            config.lint_exclude.runtime_paths(),
            ["**/tests/**", "crates/kithara-devtools/**"]
        );
    }

    #[test]
    fn style_keeps_the_tooling_globs_this_repo_excludes_from_architecture() {
        let config = ProjectConfig::load(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .as_path(),
        )
        .expect("the repo config loads");

        assert!(
            !config
                .lint_exclude
                .paths
                .iter()
                .any(|p| p.contains("kithara-devtools")),
            "a devtools glob in `paths` would hide the crate from `style` too"
        );
        assert!(
            config
                .lint_exclude
                .runtime_paths()
                .iter()
                .any(|p| p.contains("kithara-devtools")),
            "architecture and idiom rules do not apply to build tooling"
        );
    }

    #[test]
    fn quality_assessment_tool_cannot_be_configured_and_not_applicable() {
        let error = load(
            r#"
[[quality.assessment.not_applicable_tools]]
tool = "cargo-mutants"
reason = "not actionable for this workspace"

[[quality.assessment.deep_stages]]
name = "mutation"
command = ["cargo", "mutants"]
tools = ["cargo-mutants"]
"#,
        )
        .expect_err("conflicting tool policy");

        assert!(
            error
                .to_string()
                .contains("cannot be both configured and not applicable")
        );
    }

    #[test]
    fn a_tools_table_reaches_the_project_config() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir(temp.path().join(".config")).expect("create .config");
        fs::write(
            temp.path().join(".config/xtask.toml"),
            r#"
[tools.ast-grep]
program = "ast-grep"
pin = "ast-grep"
"#,
        )
        .expect("write config");

        let config = ProjectConfig::load(temp.path()).expect("the config loads");

        assert_eq!(config.tools.program("ast-grep"), "ast-grep");
    }

    /// Every moved value keeps today's setting, so an unconfigured project
    /// behaves exactly as the constants did. These four structs take
    /// `#[serde(default)]` at the container, which makes this impl the
    /// authority for every key a project does not spell out.
    #[test]
    fn the_moved_defaults_match_the_constants_they_replace() {
        let config = ProjectConfig::default();

        assert_eq!(
            config.workspace_scan.top_level_dirs,
            ["tests", "xtask", "benches"]
        );
        assert_eq!(config.health.logs_dir, "target/health-logs");
        assert_eq!(config.health.report_path, "target/health-report.md");
        assert_eq!(config.health.stdout_tail_lines, 80);
        assert_eq!(config.orphans.max_parallelism, 4);
        assert_eq!(config.architecture.runtime.semantic_timeout_secs, 120);
    }

    /// Every moved budget keeps today's setting, so a project that configures
    /// none of them renders exactly what the constants rendered.
    #[test]
    fn the_render_budget_defaults_match_the_constants_they_replace() {
        let config = ProjectConfig::default();

        assert_eq!(config.stress.render.failure_rows, 100);
        assert_eq!(config.stress.render.problem_rows, 100);
        assert_eq!(config.stress.render.iterations_per_test, 20);
        assert_eq!(config.stress.render.cell_chars, 240);
        assert_eq!(config.stress.render.finding_rows, 100);
        assert_eq!(config.stress.render.signature_rows, 100);
        assert_eq!(config.stress.render.signature_examples, 5);
        assert_eq!(config.stress.render.divergence_rows, 40);
        assert_eq!(config.stress.render.pass_only_rows_per_test, 5);
        assert_eq!(config.quality.render.findings, 100);
        assert_eq!(config.quality.render.architecture_hotspots, 30);
        assert_eq!(config.quality.render.summary_rows, 5);
        assert_eq!(config.architecture.render.findings, 12);
        assert_eq!(config.architecture.render.relations, 20);
    }
}
