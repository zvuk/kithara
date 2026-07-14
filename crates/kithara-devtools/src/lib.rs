#[cfg(feature = "lint")]
pub mod arch;
pub mod ast_grep;
#[cfg(feature = "lint")]
pub mod audit;
pub mod audit_clippy;
pub mod common;
pub mod ctx;
pub mod format;
pub mod health;
#[cfg(feature = "lint")]
pub mod idioms;
pub mod init;
#[cfg(feature = "lint")]
pub mod lint;
pub mod manifest;
pub mod orphans;
pub mod perf;
pub mod perf_compare;
pub mod quality;
pub mod scope;
pub mod similarity;
mod stages;
#[cfg(feature = "lint")]
pub mod style;
pub mod test;
pub mod typos;
pub mod util;
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
    /// Thin wrapper around `similarity-rs` with audit/advisory/strict profiles.
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
    /// Test-suite performance measurement pipeline (matrix/slow/profile/report/trace).
    Perf(perf::PerfArgs),
    /// Code quality checks.
    Quality {
        #[command(subcommand)]
        command: quality::QualityCommand,
    },
    /// Translate scope tokens to tool-specific flags (used by `just audit`).
    Scope(scope::ScopeArgs),
    /// Run workspace tests through `cargo nextest`.
    Test(test::TestArgs),
    /// Comprehensive workspace health check with markdown report.
    Health(health::HealthArgs),
    #[cfg(feature = "viz")]
    /// Architecture visualization tools (hierarchy, arc-map).
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
        CoreCommand::Perf(args) => perf::run(args, ctx),
        CoreCommand::Quality { command } => quality::run(command),
        CoreCommand::Scope(args) => scope::run(args),
        CoreCommand::Test(args) => test::run(args),
        CoreCommand::Health(args) => health::run(args),
        #[cfg(feature = "viz")]
        CoreCommand::Viz(args) => viz::run(args),
    }
}
