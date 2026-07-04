pub mod ast_grep;
pub mod common;
pub mod ctx;
pub mod format;
pub mod manifest;
pub mod orphans;
pub mod similarity;
pub mod typos;
pub mod util;

pub use ctx::Ctx;

#[derive(Debug, clap::Subcommand)]
pub enum CoreCommand {
    /// Format Rust, manifests, TOML, JSON, and Markdown through project tooling.
    Format(format::FormatArgs),
    /// Thin wrapper around `ast-grep scan` that bakes in the policy filter list.
    AstGrep(ast_grep::AstGrepArgs),
    /// Thin wrapper around `typos` that pins the workspace config.
    Typos(typos::TyposArgs),
    /// Thin wrapper around `similarity-rs` with audit/advisory/strict profiles.
    Similarity(similarity::SimilarityArgs),
    /// Cargo manifest hygiene checks.
    Manifest(manifest::ManifestArgs),
    /// Per-package `cargo modules orphans` with `--cfg-test`.
    Orphans(orphans::OrphansArgs),
}

/// Runs a core xtask command.
///
/// # Errors
///
/// Returns an error when the selected command fails.
pub fn run(cmd: &CoreCommand, ctx: &Ctx) -> anyhow::Result<()> {
    match cmd {
        CoreCommand::Format(args) => format::run(args, ctx),
        CoreCommand::AstGrep(args) => ast_grep::run(args, ctx),
        CoreCommand::Typos(args) => typos::run(args, ctx),
        CoreCommand::Similarity(args) => similarity::run(args, ctx),
        CoreCommand::Manifest(args) => manifest::run(args, ctx),
        CoreCommand::Orphans(args) => orphans::run(args, ctx),
    }
}
