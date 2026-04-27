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
