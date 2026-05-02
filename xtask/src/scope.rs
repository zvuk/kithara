//! `cargo xtask scope` — argument resolver for shell substitution.
//!
//! Translates user-friendly scope tokens (`kithara-queue`, `tests/tests`,
//! `crates/kithara-abr/src`) into the flag set that a downstream tool
//! (xtask lint / clippy / fmt / ast-grep) expects. Used by `justfile`
//! recipes (`audit`, etc.) to keep bash thin while keeping the parsing
//! logic in Rust (single source of truth, unit-tested).
//!
//! Output: one line on stdout, space-separated tokens, ready for shell
//! command substitution. Exit code 2 on unknown scope.

use anyhow::Result;
use cargo_metadata::MetadataCommand;
use clap::Args;

use crate::common::scope::{Scope, Tool};

#[derive(Debug, Args)]
pub(crate) struct ScopeArgs {
    /// Downstream tool whose flag dialect should be emitted.
    #[arg(long = "for", value_enum)]
    pub for_tool: Tool,
    /// Zero or more scope tokens. Empty = whole workspace.
    pub tokens: Vec<String>,
}

pub(crate) fn run(args: &ScopeArgs) -> Result<()> {
    let metadata = build_metadata()?;
    let workspace_root = metadata.workspace_root.as_std_path();
    let scope = Scope::resolve(&args.tokens, workspace_root)?;
    let flags = scope.flags_for(args.for_tool);
    println!("{}", flags.join(" "));
    Ok(())
}

fn build_metadata() -> Result<cargo_metadata::Metadata> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    Ok(metadata)
}
