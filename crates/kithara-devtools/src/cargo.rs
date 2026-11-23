//! Loader paths for native executables produced by Cargo.

use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

/// Build the loader environment entry for a Cargo executable.
///
/// `target_libdir` is its compiler's `rustc --print target-libdir` result.
/// Existing search paths are retained.
///
/// # Errors
/// Returns an error for relative artifact or sysroot paths, a missing profile
/// directory, or paths that cannot be joined into an environment variable.
pub fn library_path(executable: &Path, target_libdir: &Path) -> Result<(&'static str, OsString)> {
    ensure!(
        executable.is_absolute(),
        "Cargo executable path must be absolute"
    );
    ensure!(
        target_libdir.is_absolute(),
        "rustc target library directory must be absolute"
    );
    let directory = executable.parent().context("Cargo executable directory")?;
    let profile = match directory.file_name().and_then(OsStr::to_str) {
        Some("deps" | "examples") => directory.parent().context("Cargo profile directory")?,
        _ => directory,
    };
    let variable = if cfg!(target_os = "macos") {
        "DYLD_FALLBACK_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut paths = vec![
        profile.join("deps"),
        profile.to_path_buf(),
        target_libdir.into(),
    ];
    if let Some(existing) = env::var_os(variable) {
        paths.extend(env::split_paths(&existing));
    } else if cfg!(target_os = "macos") {
        if let Some(home) = env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("lib"));
        }
        paths.extend([PathBuf::from("/usr/local/lib"), PathBuf::from("/usr/lib")]);
    }
    Ok((variable, env::join_paths(paths)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_path_uses_the_artifact_profile_and_preserves_inherited_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        let profile = root.path().join("target/custom profile");
        let libdir = root.path().join("toolchain/lib");
        for relative in ["program", "deps/program-hash", "examples/program"] {
            let (variable, libraries) = library_path(&profile.join(relative), &libdir)?;
            let paths = env::split_paths(&libraries).collect::<Vec<_>>();
            assert_eq!(
                &paths[..3],
                &[profile.join("deps"), profile.clone(), libdir.clone()]
            );
            if let Some(existing) = env::var_os(variable) {
                assert_eq!(&paths[3..], env::split_paths(&existing).collect::<Vec<_>>());
            } else if cfg!(target_os = "macos") {
                assert!(
                    paths.ends_with(&[PathBuf::from("/usr/local/lib"), PathBuf::from("/usr/lib")])
                );
            }
        }
        Ok(())
    }

    #[test]
    fn loader_path_rejects_relative_artifact_and_sysroot_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert!(library_path(Path::new("program"), root.path()).is_err());
        assert!(library_path(&root.path().join("program"), Path::new("")).is_err());
        Ok(())
    }
}
