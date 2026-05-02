use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

/// Check that an external tool is available.
pub(crate) fn check_tool(tool: &str, args: &[&str], install_hint: &str) -> Result<()> {
    let ok = Command::new(tool)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
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
pub(crate) fn ensure_clean_tree(allow_dirty: bool, scope_label: &str) -> Result<()> {
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
pub(crate) fn check_rust_target(target: &str) -> Result<bool> {
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
