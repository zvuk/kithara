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

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{arch, idioms, style};

#[derive(Debug, Args)]
pub(crate) struct LintArgs {
    #[command(subcommand)]
    pub command: Option<LintCommand>,
    /// When no subcommand is given, restrict the run to specific crates.
    /// Repeatable. Forwarded to arch+style+idioms.
    #[arg(long = "crate", value_name = "NAME", global = true)]
    pub crates: Vec<String>,
    /// When no subcommand is given, restrict the run to specific paths.
    /// Repeatable. Forwarded to arch+style+idioms.
    #[arg(long = "path", value_name = "PATH", global = true)]
    pub paths: Vec<PathBuf>,
    /// When no subcommand is given, apply each namespace's autofix where
    /// available (currently style only). Forwarded as `--fix` to style.
    #[arg(long, global = true)]
    pub fix: bool,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits.
    #[arg(long = "allow-dirty", global = true)]
    pub allow_dirty: bool,
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
        None => run_all(&args.crates, &args.paths, args.fix, args.allow_dirty),
    }
}

fn run_all(crates: &[String], paths: &[PathBuf], fix: bool, allow_dirty: bool) -> Result<()> {
    let mut failures: Vec<&'static str> = Vec::new();
    // Mirror the clap `default_value` for `config_dir`; `Default::default()`
    // on `PathBuf` yields "" which silently bypasses config loading.
    let arch_args = arch::ArchArgs {
        config_dir: ".config/arch".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
        ..arch::ArchArgs::default()
    };
    let style_args = style::StyleArgs {
        config_dir: ".config/style".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
        fix,
        allow_dirty,
        ..style::StyleArgs::default()
    };
    let idioms_args = idioms::IdiomsArgs {
        config_dir: ".config/idioms".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
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
