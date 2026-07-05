use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail};

use crate::Ctx;

/// Project name from `.config/xtask.toml` (`[project] name`), used to namespace
/// scratch / cache / temp dirs so the reusable xtask carries no project-name
/// literals. Resolved once from the Cargo workspace; falls back to `xtask`
/// when unreadable.
pub fn project_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let resolved = Ctx::load()
            .map(|ctx| ctx.config.project.name)
            .unwrap_or_default();
        if resolved.is_empty() {
            "xtask".to_string()
        } else {
            resolved
        }
    })
    .as_str()
}

/// Check that an external tool is available.
///
/// # Errors
///
/// Returns an error if `tool` cannot be executed successfully with `args`.
pub fn check_tool(tool: &str, args: &[&str], install_hint: &str) -> Result<()> {
    let ok = Command::new(tool)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !ok {
        bail!("{tool} not found. Install with: {install_hint}");
    }
    Ok(())
}

/// Refuse to proceed with destructive autofix when there are uncommitted
/// changes in the working tree. Mirrors `cargo fmt` / `cargo fix` UX:
/// the user can override with `--allow-dirty` once they understand that
/// our edits will mix with their own.
///
/// `scope_label` identifies the caller in the error message (e.g. "typos").
///
/// # Errors
///
/// Returns an error if the git status command fails or the worktree is dirty
/// while `allow_dirty` is false.
pub fn ensure_clean_tree(allow_dirty: bool, scope_label: &str) -> Result<()> {
    if allow_dirty {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run `git status --porcelain`")?;
    if !output.status.success() {
        bail!(
            "`git status --porcelain` failed (exit {:?}); cannot verify working tree is clean",
            output.status.code()
        );
    }
    if !output.stdout.is_empty() {
        let preview: String = String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{scope_label} --fix refuses to run on a dirty working tree.\n\
             Commit or stash your changes first, or pass --allow-dirty to mix our edits in.\n\
             First few uncommitted entries:\n{preview}"
        );
    }
    Ok(())
}

/// Check whether a Rust target is installed (via `rustup`).
///
/// # Errors
///
/// Returns an error if `rustup target list --installed` cannot be executed or
/// its output cannot be read.
pub fn check_rust_target(target: &str) -> Result<bool> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("failed to run rustup target list --installed")?;
    let reader = BufReader::new(output.stdout.as_slice());
    for line in reader.lines() {
        if line.context("reading rustup output")? == target {
            return Ok(true);
        }
    }
    Ok(false)
}
