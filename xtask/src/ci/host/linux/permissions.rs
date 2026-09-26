//! File modes, stated once for a module that compiles everywhere.
//!
//! This module provisions a Linux machine and refuses to run anywhere else,
//! the way [`super::host`]'s macOS half does. Compiling is the other half:
//! `just check` checks the whole workspace on every platform the project has a
//! lane for, and `std::os::unix` is not there on Windows, so naming it inline
//! stopped that lane at this crate before it reached a test.

use std::path::Path;

use anyhow::{Context, Result};

/// Give `path` the mode a Unix machine would enforce.
///
/// Off Unix there is no mode to give: the provisioning this serves never runs
/// there, and a file the check-only lane never creates has nothing to protect.
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    apply(path, mode).with_context(|| format!("restricting {}", path.display()))
}

#[cfg(unix)]
fn apply(path: &Path, mode: u32) -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt};

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn apply(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}
