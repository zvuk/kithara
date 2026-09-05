use std::process::Command;

use anyhow::Result;
use clap::Args;

use crate::{
    Ctx,
    util::{check_tool, ensure_clean_tree},
    verdict::NotClean,
};

struct Consts;
impl Consts {
    const CONFIG_PATH: &'static str = ".config/typos.toml";
    const INSTALL_HINT: &'static str = "cargo install typos-cli";
}

#[derive(Debug, Args)]
pub struct TyposArgs {
    /// Optional paths to scan. Empty = whole workspace (typos default).
    pub paths: Vec<String>,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits. Mirrors `cargo fmt`/`cargo fix` UX.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Apply suggested fixes by passing `--write-changes` to typos.
    /// Refuses to run on a dirty working tree unless `--allow-dirty`.
    #[arg(long)]
    pub fix: bool,
}

pub(crate) fn run(args: &TyposArgs, ctx: &Ctx) -> Result<()> {
    let program = ctx.config.tools.program("typos");
    check_tool(
        program,
        &["--version"],
        ctx.config.tools.install_hint("typos", Consts::INSTALL_HINT),
    )?;
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "typos")?;
    }
    let mut cmd = Command::new(program);
    cmd.arg("--config")
        .arg(Consts::CONFIG_PATH)
        .arg("--isolated");
    if args.fix {
        cmd.arg("--write-changes");
    }
    for p in &args.paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(NotClean::reported("typos"));
    }
    Ok(())
}
