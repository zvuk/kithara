//! `cargo xtask typos` — thin wrapper around the `typos` CLI that
//! pins the workspace config and isolates from ambient discovery.
//!
//! Single source of truth for typos invocation; used by `just typos`,
//! `just audit`, and `just health` so the config path lives in one place.

use std::process::Command;

use anyhow::{Result, bail};
use clap::Args;

use crate::util::check_tool;

struct Consts;
impl Consts {
    const CONFIG_PATH: &'static str = ".config/typos.toml";
    const INSTALL_HINT: &'static str = "cargo install typos-cli";
}

#[derive(Debug, Args)]
pub(crate) struct TyposArgs {
    /// Optional paths to scan. Empty = whole workspace (typos default).
    pub paths: Vec<String>,
}

pub(crate) fn run(args: &TyposArgs) -> Result<()> {
    check_tool("typos", &["--version"], Consts::INSTALL_HINT)?;
    let mut cmd = Command::new("typos");
    cmd.arg("--config")
        .arg(Consts::CONFIG_PATH)
        .arg("--isolated");
    for p in &args.paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("typos failed (exit code {:?})", status.code());
    }
    Ok(())
}
