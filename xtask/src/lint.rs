use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use kithara_xtask_core::common::style::bold_cyan;

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

    println!("{}", bold_cyan("══ arch ══"));
    if arch::run(&arch_args).is_err() {
        failures.push("arch");
    }
    println!("\n{}", bold_cyan("══ style ══"));
    if style::run(&style_args).is_err() {
        failures.push("style");
    }
    println!("\n{}", bold_cyan("══ idioms ══"));
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
