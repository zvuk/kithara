#[cfg(feature = "lint")]
pub mod arch;
pub mod ast_grep;
#[cfg(feature = "lint")]
pub mod audit;
pub mod audit_clippy;
pub mod ci_report;
pub mod common;
pub mod ctx;
pub mod format;
pub mod health;
#[cfg(feature = "lint")]
pub mod idioms;
pub mod init;
pub mod junit;
pub mod lease;
#[cfg(feature = "lint")]
pub mod lint;
pub mod lock;
pub mod manifest;
pub mod orphans;
pub mod perf;
pub mod perf_compare;
pub mod powerset;
pub mod quality;
pub mod quality_assessment;
pub mod quality_lab;
pub mod scope;
pub mod semver;
pub mod similarity;
mod stages;
pub mod stress;
mod stress_report;
mod stress_run;
#[cfg(feature = "lint")]
pub mod style;
pub mod test;
pub mod typos;
pub mod util;
pub mod verdict;
#[cfg(feature = "viz")]
pub mod viz;

pub use ctx::Ctx;

#[derive(Debug, clap::Subcommand)]
pub enum CoreCommand {
    /// Scaffold the workspace tooling config and lint baselines.
    Init(init::InitArgs),
    #[cfg(feature = "lint")]
    /// Run the scoped formatter, lint, typo, similarity, and orphan audit.
    Audit(audit::AuditArgs),
    /// Format Rust, manifests, TOML, JSON, and Markdown through project tooling.
    Format(format::FormatArgs),
    /// Thin wrapper around `ast-grep scan` that bakes in the policy filter list.
    AstGrep(ast_grep::AstGrepArgs),
    /// Opt-in, non-gating Clippy sweep for extended advisory lints.
    AuditClippy(audit_clippy::AuditClippyArgs),
    /// Thin wrapper around `typos` that pins the workspace config.
    Typos(typos::TyposArgs),
    /// Analyze structural and behavioral similarity, then run similarity-rs.
    Similarity(similarity::SimilarityArgs),
    /// Cargo manifest hygiene checks.
    Manifest(manifest::ManifestArgs),
    /// Per-package `cargo modules orphans` with `--cfg-test`.
    Orphans(orphans::OrphansArgs),
    #[cfg(feature = "lint")]
    /// Workspace linters: arch, style, idioms (run all, or one via subcommand).
    Lint(lint::LintArgs),
    /// Compare perf results.
    PerfCompare(perf_compare::PerfCompareArgs),
    /// Check every feature combination the workspace admits.
    Powerset(powerset::PowersetArgs),
    /// Test-suite performance measurement pipeline (matrix/slow/profile/report/trace).
    Perf(perf::PerfArgs),
    /// Code quality checks.
    Quality {
        #[command(subcommand)]
        command: quality::QualityCommand,
    },
    /// Translate scope tokens to tool-specific flags (used by `just ci audit`).
    Scope(scope::ScopeArgs),
    /// Compare the workspace's public surface against a baseline revision.
    Semver(semver::SemverArgs),
    /// Run workspace tests through `cargo nextest`.
    Test(test::TestArgs),
    /// Execute or independently verify a repeated-test evidence run.
    Stress {
        #[command(subcommand)]
        command: stress::StressCommand,
    },
    /// Comprehensive workspace health check with markdown report.
    Health(health::HealthArgs),
    /// Render one consolidated report from a run's quality artifacts.
    CiReport(ci_report::CiReportArgs),
    #[cfg(feature = "viz")]
    /// Build architecture views from shared source and runtime evidence.
    Viz(viz::VizArgs),
}

/// Runs a core xtask command.
///
/// # Errors
///
/// Returns an error when the selected command fails.
pub fn run(cmd: &CoreCommand, ctx: &Ctx) -> anyhow::Result<()> {
    match cmd {
        CoreCommand::Init(args) => init::run(args, ctx),
        #[cfg(feature = "lint")]
        CoreCommand::Audit(args) => audit::run(args, ctx),
        CoreCommand::Format(args) => format::run(args, ctx),
        CoreCommand::AstGrep(args) => ast_grep::run(args, ctx),
        CoreCommand::AuditClippy(args) => audit_clippy::run(args, ctx),
        CoreCommand::Typos(args) => typos::run(args, ctx),
        CoreCommand::Similarity(args) => similarity::run(args, ctx),
        CoreCommand::Manifest(args) => manifest::run(args, ctx),
        CoreCommand::Orphans(args) => orphans::run(args, ctx),
        #[cfg(feature = "lint")]
        CoreCommand::Lint(args) => lint::run(args),
        CoreCommand::PerfCompare(args) => perf_compare::run(args),
        CoreCommand::Powerset(args) => powerset::run(args, ctx),
        CoreCommand::Perf(args) => perf::run(args, ctx),
        CoreCommand::Quality { command } => quality::run(command, ctx),
        CoreCommand::Scope(args) => scope::run(args),
        CoreCommand::Semver(args) => semver::run(args, ctx),
        CoreCommand::Test(args) => test::run(args),
        CoreCommand::Stress { command } => stress::run(command, ctx),
        CoreCommand::Health(args) => health::run(args),
        CoreCommand::CiReport(args) => ci_report::run(args, ctx),
        #[cfg(feature = "viz")]
        CoreCommand::Viz(args) => viz::run(args, ctx),
    }
}
