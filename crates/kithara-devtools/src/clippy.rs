use std::process::Command;

use anyhow::{Context, Result};

use crate::{Ctx, sccache, verdict::ChildFailure};

/// Run the workspace Clippy gate with the caching that suits where it runs.
///
/// The recipe used to state one caching policy for both, which is one policy too
/// few: see [`sccache::clippy_cleared`] for why a workstation and a CI job want
/// opposite halves of a trade they cannot both have.
///
/// The gate lints every target, not only the libraries and binaries Cargo picks
/// by default: `#[cfg(test)]` modules, integration tests, benches and examples
/// compile only under their own targets, and the lint policy holds them to the
/// same standard.
///
/// Native product features come from the selected project's configuration.
///
/// # Errors
///
/// Returns an error when Clippy cannot be started, or the child's own exit code
/// when it reports a lint.
pub(crate) fn run(ctx: &Ctx) -> Result<()> {
    let mut cmd = Command::new(ctx.config.tools.program("cargo"));
    cmd.current_dir(&ctx.root);
    cmd.args(["clippy", "--workspace", "--all-targets"]);
    if cfg!(target_os = "macos") && !ctx.config.clippy.macos_features.is_empty() {
        cmd.args(["--features", &ctx.config.clippy.macos_features.join(",")]);
    }
    cmd.args(["--", "-D", "warnings"]);
    for name in sccache::clippy_cleared() {
        cmd.env_remove(name);
    }
    let status = cmd.status().context("failed to run `cargo clippy`")?;
    if !status.success() {
        return Err(ChildFailure::inherited("clippy".to_owned(), status.code()));
    }
    Ok(())
}
