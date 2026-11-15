use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    Ctx, arch, arch::ArchArgs, common::style::bold_cyan, idioms, idioms::IdiomsArgs, style,
    style::StyleArgs,
};

#[derive(Debug, Args)]
pub struct LintArgs {
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
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits.
    #[arg(long = "allow-dirty", global = true)]
    pub allow_dirty: bool,
    /// When no subcommand is given, apply each namespace's autofix where
    /// available. Forwarded as `--fix` to style and idioms.
    #[arg(long, global = true)]
    pub fix: bool,
}

#[derive(Debug, Subcommand)]
pub enum LintCommand {
    /// Architectural fitness functions (topology, layers, file size, …).
    Arch(ArchArgs),
    /// Code-style fitness functions (const locality, field/item ordering, …).
    Style(StyleArgs),
    /// Idiomatic-construction fitness functions (branch chains, accumulators, …).
    Idioms(IdiomsArgs),
}

pub(crate) fn run(args: &LintArgs, ctx: &Ctx) -> Result<()> {
    match &args.command {
        Some(LintCommand::Arch(a)) => arch::run(a, ctx),
        Some(LintCommand::Style(a)) => style::run(a, ctx),
        Some(LintCommand::Idioms(a)) => idioms::run(a, ctx),
        None => run_all(&args.crates, &args.paths, args.fix, args.allow_dirty, ctx),
    }
}

fn run_all(
    crates: &[String],
    paths: &[PathBuf],
    fix: bool,
    allow_dirty: bool,
    ctx: &Ctx,
) -> Result<()> {
    let mut failures = Vec::new();
    let arch_args = ArchArgs {
        config_dir: ".config/arch".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
        ..ArchArgs::default()
    };
    let style_args = StyleArgs {
        config_dir: ".config/style".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
        fix,
        allow_dirty,
        ..StyleArgs::default()
    };
    let idioms_args = IdiomsArgs {
        config_dir: ".config/idioms".into(),
        crates: crates.to_vec(),
        paths: paths.to_vec(),
        fix,
        allow_dirty,
        ..IdiomsArgs::default()
    };

    println!("{}", bold_cyan("══ arch ══"));
    if let Err(error) = arch::run(&arch_args, ctx) {
        failures.push(format!("arch: {error:#}"));
    }
    println!("\n{}", bold_cyan("══ style ══"));
    if let Err(error) = style::run(&style_args, ctx) {
        failures.push(format!("style: {error:#}"));
    }
    println!("\n{}", bold_cyan("══ idioms ══"));
    if let Err(error) = idioms::run(&idioms_args, ctx) {
        failures.push(format!("idioms: {error:#}"));
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
