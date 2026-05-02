//! `cargo xtask typos` — thin wrapper around the `typos` CLI that
//! pins the workspace config and isolates from ambient discovery.
//!
//! Single source of truth for typos invocation; used by `just typos`,
//! `just audit`, and `just health` so the config path lives in one place.

use std::process::Command;

use anyhow::{Result, bail};
use clap::Args;

use crate::util::{check_tool, ensure_clean_tree};

struct Consts;
impl Consts {
    const CONFIG_PATH: &'static str = ".config/typos.toml";
    const INSTALL_HINT: &'static str = "cargo install typos-cli";
}

#[derive(Debug, Args)]
pub(crate) struct TyposArgs {
    /// Apply suggested fixes by passing `--write-changes` to typos.
    /// Refuses to run on a dirty working tree unless `--allow-dirty`.
    #[arg(long)]
    pub fix: bool,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits. Mirrors `cargo fmt`/`cargo fix` UX.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Optional paths to scan. Empty = whole workspace (typos default).
    pub paths: Vec<String>,
}

pub(crate) fn run(args: &TyposArgs) -> Result<()> {
    check_tool("typos", &["--version"], Consts::INSTALL_HINT)?;
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "typos")?;
    }
    let mut cmd = Command::new("typos");
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
        bail!("typos failed (exit code {:?})", status.code());
    }
    Ok(())
}
