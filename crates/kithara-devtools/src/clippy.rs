use std::process::Command;

use anyhow::{Context, Result};

use crate::{sccache, verdict::ChildFailure};

/// Run the workspace Clippy gate with the caching that suits where it runs.
///
/// The recipe used to state one caching policy for both, which is one policy too
/// few: see [`sccache::clippy_cleared`] for why a workstation and a CI job want
/// opposite halves of a trade they cannot both have.
///
/// # Errors
///
/// Returns an error when Clippy cannot be started, or the child's own exit code
/// when it reports a lint.
pub(crate) fn run() -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.args(["clippy", "--workspace", "--", "-D", "warnings"]);
    for name in sccache::clippy_cleared() {
        cmd.env_remove(name);
    }
    let status = cmd.status().context("failed to run `cargo clippy`")?;
    if !status.success() {
        return Err(ChildFailure::inherited("clippy".to_owned(), status.code()));
    }
    Ok(())
}
