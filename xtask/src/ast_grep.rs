//! `cargo xtask ast-grep` — thin wrapper around the `ast-grep` CLI that
//! bakes in the blocking/advisory filter list.
//!
//! Single source of truth for which ast-grep rules are blocking on
//! pre-commit (`just lint-fast`) vs warning-only (`just ast-grep advisory`).
//! Used by both `just ast-grep` and `just audit` so the filter list
//! lives in one place.

use std::process::Command;

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};

use crate::util::ensure_clean_tree;

const BLOCKING_FILTER: &str = concat!(
    "^(",
    "rust.no-lint-suppression",
    "|rust.no-crate-lint-allow",
    "|style.no-items-in-lib-or-mod-rs",
    "|rust.no-thin-async-wrapper",
    "|style.no-separator-comments-toml",
    "|style.no-noop-in-impl",
    "|style.no-duplicate-impl",
    "|style.no-masked-unused-arg",
    "|style.multiple-private-module-consts",
    "|style.no-impl-only-consts",
    "|rust.no-error-string-match",
    ")$",
);

#[derive(Debug, Args)]
pub(crate) struct AstGrepArgs {
    #[arg(long, value_enum, default_value_t = Mode::Blocking)]
    pub mode: Mode,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Mode {
    /// CI-deny rule subset (default). Used by `just lint-fast` and `just audit`.
    Blocking,
    /// Full warning-level set. Used by `just ast-grep advisory`.
    Advisory,
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
    match args.mode {
        Mode::Blocking => {
            cmd.arg("--filter").arg(BLOCKING_FILTER);
        }
        Mode::Advisory => {
            cmd.arg("--warning");
        }
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
