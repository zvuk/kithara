//! `cargo xtask ast-grep` — thin wrapper around the `ast-grep` CLI.
//!
//! Discovers every rule under `.config/ast-grep/` and lets each rule's
//! own `severity:` field decide whether a hit blocks (`error`) or just
//! reports (`warning`/`info`/`hint`). No per-id filter list lives in
//! Rust — adding a new rule file is enough.
//!
//! Pass `--strict` to promote every warning to an error (used by
//! exhaustive sweeps, not by the default audit).

use std::process::Command;

use anyhow::{Result, bail};
use clap::Args;

use crate::util::ensure_clean_tree;

#[derive(Debug, Args)]
pub(crate) struct AstGrepArgs {
    /// Promote every warning to an error (passes `--warning` to ast-grep).
    /// Use for exhaustive sweeps when warning-level rules should also fail.
    #[arg(long)]
    pub strict: bool,
    /// Apply rule fixes by passing `--update-all` to ast-grep. Only rules
    /// that declare a `fix:` block in `.config/ast-grep/*.yml` actually
    /// rewrite anything; rules without one stay reporting-only.
    /// Refuses to run on a dirty working tree unless `--allow-dirty`.
    #[arg(long)]
    pub fix: bool,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits. Mirrors `cargo fmt`/`cargo fix` UX.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Optional paths to scan. Empty = whole workspace.
    pub paths: Vec<String>,
}

pub(crate) fn run(args: &AstGrepArgs) -> Result<()> {
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "ast-grep")?;
    }
    let mut cmd = Command::new("ast-grep");
    cmd.arg("scan")
        .arg("--config")
        .arg("sgconfig.yml")
        .arg("--report-style")
        .arg("short");
    if args.strict {
        cmd.arg("--warning");
    }
    if args.fix {
        cmd.arg("--update-all");
    }
    for p in &args.paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("ast-grep failed (exit code {:?})", status.code());
    }
    Ok(())
}
