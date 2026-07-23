use std::{
    io::{Write, stdout},
    process::Command,
};

use anyhow::Result;
use clap::{Args, FromArgMatches, Subcommand};

use crate::{
    CoreCommand, Ctx, ast_grep,
    common::{
        scope::Scope,
        style::{bold, bold_cyan, bold_green, bold_red, dim},
    },
    lint,
    stages::SharedStage,
    typos,
};

#[derive(Debug, Args)]
pub struct AuditArgs {
    /// Crate names or workspace-relative paths to audit.
    #[arg(value_name = "SCOPE")]
    pub scope: Vec<String>,
    /// Apply all supported fixes before the read-only audit.
    #[arg(long)]
    pub autofix: bool,
}

#[derive(Clone, Copy)]
enum Status {
    Ok,
    Fail,
    Skip,
}

pub(crate) fn run(args: &AuditArgs, ctx: &Ctx) -> Result<()> {
    if args.autofix {
        run_autofix(ctx);
    }

    let scope = match Scope::resolve(&args.scope, &ctx.root) {
        Ok(scope) => scope,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    };
    let mut results = Vec::with_capacity(SharedStage::AUDIT.len());

    for stage in SharedStage::AUDIT {
        section(stage.audit_section());
        let command = stage.audit_command(&scope);
        let status = if stage == SharedStage::Orphans
            && command.args.last().is_some_and(|arg| arg == "__skip__")
        {
            println!("orphans: skipped (non-crate scope)");
            Status::Skip
        } else {
            match run_stage(command.program, &command.args, ctx) {
                Ok(()) => Status::Ok,
                Err(error) => {
                    if command.program == "xtask" {
                        eprintln!("Error: {error:#}");
                    }
                    Status::Fail
                }
            }
        };
        results.push((stage.audit_name(), status));
    }

    print_summary(&results);
    stdout().flush()?;
    if results
        .iter()
        .any(|(_, status)| matches!(status, Status::Fail))
    {
        println!("{}", bold_red("audit: FAILED"));
        std::process::exit(1);
    }
    println!("{}", bold_green("audit: OK"));
    Ok(())
}

fn run_stage(program: &str, args: &[String], ctx: &Ctx) -> Result<()> {
    if program == "xtask" {
        let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
        let matches = command.try_get_matches_from(
            std::iter::once("xtask").chain(args.iter().map(String::as_str)),
        )?;
        let command = CoreCommand::from_arg_matches(&matches)?;
        return crate::run(&command, ctx);
    }
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{program} failed (exit code {:?})", status.code())
    }
}

fn run_autofix(ctx: &Ctx) {
    section("autofix: fmt");
    report_autofix(run_external("cargo", &["+nightly", "fmt", "--all"]));
    section("autofix: clippy (1/2)");
    report_autofix(run_external(
        "cargo",
        &[
            "clippy",
            "--fix",
            "--workspace",
            "--all-targets",
            "--allow-dirty",
            "--",
            "-D",
            "warnings",
        ],
    ));
    section("autofix: typos");
    report_autofix(typos::run(
        &typos::TyposArgs {
            paths: Vec::new(),
            allow_dirty: true,
            fix: true,
        },
        ctx,
    ));
    section("autofix: ast-grep");
    report_autofix(ast_grep::run(
        &ast_grep::AstGrepArgs {
            paths: Vec::new(),
            allow_dirty: true,
            fix: true,
            raw: false,
            strict: false,
        },
        ctx,
    ));
    section("autofix: xtask lint");
    report_autofix(lint::run(&lint::LintArgs {
        command: None,
        crates: Vec::new(),
        paths: Vec::new(),
        allow_dirty: true,
        fix: true,
    }));
    section("autofix: clippy (2/2)");
    report_autofix(run_external(
        "cargo",
        &[
            "clippy",
            "--fix",
            "--workspace",
            "--all-targets",
            "--allow-dirty",
            "--",
            "-D",
            "warnings",
        ],
    ));
    section("autofix: fmt cleanup");
    report_autofix(run_external("cargo", &["+nightly", "fmt", "--all"]));
}

fn report_autofix(result: Result<()>) {
    if let Err(error) = result {
        eprintln!("Error: {error:#}");
    }
}

fn run_external(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{program} failed (exit code {:?})", status.code())
    }
}

fn section(name: &str) {
    println!("\n{}", bold_cyan(&format!("\u{2500}\u{2500} {name}")));
}

fn print_summary(results: &[(&str, Status)]) {
    println!("\n{}", bold_cyan("\u{2500}\u{2500} summary"));
    for (name, status) in results {
        let badge = match status {
            Status::Ok => bold_green("\u{2713} ok"),
            Status::Fail => bold_red("\u{2717} FAIL"),
            Status::Skip => dim("\u{2218} skip"),
        };
        println!("  {} {badge}", bold(&format!("{name:<12}")));
    }
    println!();
}
