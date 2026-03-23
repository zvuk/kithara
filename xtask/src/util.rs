use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

/// Recursively collect all `.rs` files under `dir`, returning them sorted.
pub(crate) fn walk_rs_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    walk_rs_files_inner(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_rs_files_inner(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files_inner(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_file(path: &Path, replacements: &[(&str, &str)]) -> Result<()> {
        let mut content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for (needle, replacement) in replacements {
            content = content.replace(needle, replacement);
        }
        fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    #[test]
    fn patch_file_applies_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world, hello rust").unwrap();
        patch_file(&path, &[("hello", "hi")]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hi world, hi rust");
    }

    #[test]
    fn patch_file_no_match_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "unchanged content").unwrap();
        patch_file(&path, &[("missing", "replacement")]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "unchanged content");
    }
}
