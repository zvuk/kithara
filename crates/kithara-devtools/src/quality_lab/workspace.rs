use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::common::process::{ProcessRequest, run_process};

pub(super) fn prepare_clean_clone(
    workspace_root: &Path,
    tool_dir: &Path,
    revision: &str,
    started: Instant,
    budget: Duration,
    run_root: &mut PathBuf,
) -> Result<()> {
    let status = git_text(
        workspace_root,
        &["status", "--porcelain"],
        "verify clean worktree for Cha",
    )?;
    if !status.is_empty() {
        bail!("Cha requires a clean worktree so its history and source snapshot agree");
    }
    let shallow = git_text(
        workspace_root,
        &["rev-parse", "--is-shallow-repository"],
        "verify complete Git history for Cha",
    )?;
    if shallow != "false" {
        bail!("Cha requires a non-shallow Git history");
    }
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        bail!("tool time budget exhausted before clean clone");
    }
    let scratch = tool_dir.join("worktree");
    let args = vec![
        "clone".to_owned(),
        "--shared".to_owned(),
        "--quiet".to_owned(),
        workspace_root.to_string_lossy().into_owned(),
        scratch.to_string_lossy().into_owned(),
    ];
    let outcome = run_process(&ProcessRequest {
        program: "git",
        args: &args,
        cwd: workspace_root,
        stdout_path: &tool_dir.join("clone.stdout.log"),
        stderr_path: &tool_dir.join("clone.stderr.log"),
        timeout: remaining,
    })
    .map_err(|error| anyhow!("create isolated Cha clone: {error}"))?;
    if outcome.timed_out {
        bail!("create isolated Cha clone: timed out");
    }
    if !outcome.status.success() {
        bail!(
            "create isolated Cha clone: git exited with {:?}",
            outcome.status.code()
        );
    }
    let cloned_revision = git_text(
        &scratch,
        &["rev-parse", "HEAD"],
        "verify Cha clone revision",
    )?;
    if cloned_revision != revision {
        bail!("isolated Cha clone revision mismatch: expected {revision}, got {cloned_revision}");
    }
    *run_root = scratch;
    Ok(())
}

pub(super) fn remove_scratch_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("remove Quality Lab scratch clone: {}", path.display()))?;
    }
    Ok(())
}

pub(super) fn reset_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(path)
                .with_context(|| format!("remove stale Quality Lab output: {}", path.display()))?;
        } else {
            fs::remove_dir_all(path)
                .with_context(|| format!("remove stale Quality Lab output: {}", path.display()))?;
        }
    }
    fs::create_dir_all(path)
        .with_context(|| format!("create Quality Lab tool output: {}", path.display()))
}

pub(super) fn git_text(root: &Path, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| context.to_owned())?;
    if !output.status.success() {
        bail!("{context}: git exited with {:?}", output.status.code());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
