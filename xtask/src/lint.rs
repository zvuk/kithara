//! Unified entry point for the three workspace linters.
//!
//! - `cargo xtask lint`         — run arch, style, and idioms in sequence
//! - `cargo xtask lint arch`    — only architectural fitness functions
//! - `cargo xtask lint style`   — only code-style checks
//! - `cargo xtask lint idioms`  — only idiomatic-construction checks
//!
//! Each subcommand forwards its full arg surface (`--check`, `--report`,
//! `--json`, `--update-baseline`, `--config-dir`) to the underlying
//! namespace.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{arch, idioms, style};

#[derive(Debug, Args)]
pub(crate) struct LintArgs {
    #[command(subcommand)]
    pub command: Option<LintCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LintCommand {
    /// Architectural fitness functions (topology, layers, file size, …).
    Arch(arch::ArchArgs),
    /// Code-style fitness functions (const locality, field/item ordering, …).
    Style(style::StyleArgs),
    /// Idiomatic-construction fitness functions (branch chains, accumulators, …).
    Idioms(idioms::IdiomsArgs),
}

pub(crate) fn run(args: &LintArgs) -> Result<()> {
    match &args.command {
        Some(LintCommand::Arch(a)) => arch::run(a),
        Some(LintCommand::Style(a)) => style::run(a),
        Some(LintCommand::Idioms(a)) => idioms::run(a),
        None => run_all(),
    }
}

fn run_all() -> Result<()> {
    let mut failures: Vec<&'static str> = Vec::new();
    // Mirror the clap `default_value` for `config_dir`; `Default::default()`
    // on `PathBuf` yields "" which silently bypasses config loading.
    let arch_args = arch::ArchArgs {
        config_dir: ".config/arch".into(),
        ..arch::ArchArgs::default()
    };
    let style_args = style::StyleArgs {
        config_dir: ".config/style".into(),
        ..style::StyleArgs::default()
    };
    let idioms_args = idioms::IdiomsArgs {
        config_dir: ".config/idioms".into(),
        ..idioms::IdiomsArgs::default()
    };

    println!("== arch ==");
    if arch::run(&arch_args).is_err() {
        failures.push("arch");
    }
    println!("\n== style ==");
    if style::run(&style_args).is_err() {
        failures.push("style");
    }
    println!("\n== idioms ==");
    if idioms::run(&idioms_args).is_err() {
        failures.push("idioms");
    }

    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} lint namespace(s) failed: {}",
            failures.len(),
            failures.join(", ")
        )
    }
}
